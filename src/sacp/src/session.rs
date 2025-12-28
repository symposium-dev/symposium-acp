use std::{future::Future, marker::PhantomData, path::Path};

use agent_client_protocol_schema::{
    ContentBlock, ContentChunk, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SessionModeState, SessionNotification, SessionUpdate, StopReason,
};
use futures::channel::mpsc;
use tokio::sync::oneshot;

use crate::{
    AgentPeer, ClientPeer, Handled, HasPeer, JrConnectionCx, JrLink, JrMessageHandler, JrRequestCx,
    MessageCx,
    jsonrpc::{
        DynamicHandlerRegistration,
        responder::{ChainResponder, JrResponder, NullResponder},
    },
    link::ProxySessionMessages,
    mcp_server::McpServer,
    schema::SessionId,
    util::{MatchMessage, MatchMessageFrom, run_until},
};

/// Marker type indicating the session builder will block the current task.
#[derive(Debug)]
pub struct Blocking;
impl SessionBlockState for Blocking {}

/// Marker type indicating the session builder will not block the current task.
#[derive(Debug)]
pub struct NonBlocking;
impl SessionBlockState for NonBlocking {}

/// Trait for marker types that indicate blocking vs blocking API.
/// See [`SessionBuilder::block_task`].
pub trait SessionBlockState: Send + 'static + Sync + std::fmt::Debug {}

impl<Link: JrLink> JrConnectionCx<Link>
where
    Link: HasPeer<AgentPeer>,
{
    /// Session builder for a new session request.
    pub fn build_session(&self, cwd: impl AsRef<Path>) -> SessionBuilder<Link, NullResponder> {
        SessionBuilder::new(
            self,
            NewSessionRequest {
                cwd: cwd.as_ref().to_owned(),
                mcp_servers: Default::default(),
                meta: Default::default(),
            },
        )
    }

    /// Session builder using the current working directory.
    ///
    /// This is a convenience wrapper around [`build_session`](Self::build_session)
    /// that uses [`std::env::current_dir`] to get the working directory.
    ///
    /// Returns an error if the current directory cannot be determined.
    pub fn build_session_cwd(&self) -> Result<SessionBuilder<Link, NullResponder>, crate::Error> {
        let cwd = std::env::current_dir().map_err(|e| {
            crate::Error::internal_error().with_data(format!("cannot get current directory: {e}"))
        })?;
        Ok(self.build_session(cwd))
    }

    /// Session builder starting from an existing request.
    ///
    /// Use this when you've intercepted a `session.new` request and want to
    /// modify it (e.g., inject MCP servers) before forwarding.
    pub fn build_session_from(
        &self,
        request: NewSessionRequest,
    ) -> SessionBuilder<Link, NullResponder> {
        SessionBuilder::new(self, request)
    }

    /// Given a session response received from the agent,
    /// attach a handler to process messages related to this session
    /// and let you access them.
    ///
    /// Normally you would not use this method directly but would
    /// instead use [`Self::build_session`] and then [`SessionBuilder::start_session`].
    ///
    /// The vector `dynamic_handler_registrations` contains any dynamic
    /// handle registrations associated with this session (e.g., from MCP servers).
    /// You can simply pass `Default::default()` if not applicable.
    pub fn attach_session<'responder>(
        &self,
        response: NewSessionResponse,
        mcp_handler_registrations: Vec<DynamicHandlerRegistration<Link>>,
    ) -> Result<ActiveSession<'responder, Link>, crate::Error> {
        let NewSessionResponse {
            session_id,
            modes,
            meta,
        } = response;

        let (update_tx, update_rx) = mpsc::unbounded();
        let handler =
            ActiveSessionHandler::new(Link::default(), session_id.clone(), update_tx.clone());
        let session_handler_registration = self.add_dynamic_handler(handler)?;

        Ok(ActiveSession {
            session_id,
            modes,
            meta,
            update_rx,
            update_tx,
            connection: self.clone(),
            session_handler_registration,
            mcp_handler_registrations,
            _responder: PhantomData,
        })
    }
}

/// Session builder for a new session request.
/// Allows you to add MCP servers or set other details for this session.
///
/// The `BlockState` type parameter tracks whether blocking methods are available:
/// - `NonBlocking` (default): Only [`on_session_start`](Self::on_session_start) is available
/// - `Blocking` (after calling [`block_task`](Self::block_task)):
///   [`run_until`](Self::run_until) and [`start_session`](Self::start_session) become available
#[must_use = "use `start_session`, `run_until`, or `on_session_start` to start the session"]
pub struct SessionBuilder<
    Link,
    Responder: JrResponder<Link> = NullResponder,
    BlockState: SessionBlockState = NonBlocking,
> where
    Link: HasPeer<AgentPeer>,
{
    connection: JrConnectionCx<Link>,
    request: NewSessionRequest,
    dynamic_handler_registrations: Vec<DynamicHandlerRegistration<Link>>,
    responder: Responder,
    block_state: PhantomData<BlockState>,
}

impl<Link> SessionBuilder<Link, NullResponder, NonBlocking>
where
    Link: HasPeer<AgentPeer>,
{
    fn new(connection: &JrConnectionCx<Link>, request: NewSessionRequest) -> Self {
        SessionBuilder {
            connection: connection.clone(),
            request,
            dynamic_handler_registrations: Default::default(),
            responder: NullResponder,
            block_state: PhantomData,
        }
    }
}

impl<Link, Responder, BlockState> SessionBuilder<Link, Responder, BlockState>
where
    Link: HasPeer<AgentPeer>,
    Responder: JrResponder<Link>,
    BlockState: SessionBlockState,
{
    /// Add the MCP servers from the given registry to this session.
    pub fn with_mcp_server<R>(
        mut self,
        mcp_server: McpServer<Link, R>,
    ) -> Result<SessionBuilder<Link, ChainResponder<Responder, R>, BlockState>, crate::Error>
    where
        R: JrResponder<Link>,
    {
        let (handler, responder) = mcp_server.into_handler_and_responder();
        self.dynamic_handler_registrations
            .push(handler.into_dynamic_handler(&mut self.request, &self.connection)?);
        Ok(SessionBuilder {
            connection: self.connection,
            request: self.request,
            dynamic_handler_registrations: self.dynamic_handler_registrations,
            responder: ChainResponder::new(self.responder, responder),
            block_state: self.block_state,
        })
    }

    /// Spawn a task that runs the provided closure once the session starts.
    ///
    /// Unlike [`start_session`](Self::start_session), this method returns immediately
    /// without blocking the current task. The session handshake and closure execution
    /// happen in a spawned background task.
    ///
    /// The closure receives an `ActiveSession<'static, _>` and should return
    /// `Result<(), Error>`. If the closure returns an error, it will propagate
    /// to the connection's error handling.
    ///
    /// # Example
    ///
    /// ```ignore
    /// cx.build_session(cwd)
    ///     .with_mcp_server(mcp)?
    ///     .on_session_start(async |session| {
    ///         // Do something with the session
    ///         session.send_prompt("Hello")?;
    ///         let response = session.read_to_string().await?;
    ///         Ok(())
    ///     })?;
    /// // Returns immediately, session runs in background
    /// ```
    pub fn on_session_start<F, Fut>(self, op: F) -> Result<(), crate::Error>
    where
        Responder: 'static,
        F: FnOnce(ActiveSession<'static, Link>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), crate::Error>> + Send,
    {
        let connection = self.connection.clone();
        connection.spawn(async move {
            let Self {
                connection,
                request,
                dynamic_handler_registrations,
                responder,
                block_state: _,
            } = self;

            let response = connection
                .send_request_to(AgentPeer, request)
                .block_task()
                .await?;

            connection.spawn(responder.run(connection.clone()))?;

            let active_session =
                connection.attach_session(response, dynamic_handler_registrations)?;

            op(active_session).await
        })
    }

    /// Spawn a session proxy and run a closure with the session ID.
    ///
    /// Unlike [`start_session_proxy`](Self::start_session_proxy), this method returns
    /// immediately without blocking the current task. The session handshake, client
    /// response, and proxy setup all happen in a spawned background task.
    ///
    /// The closure receives the `SessionId` once the session is established, allowing
    /// you to perform any custom work with that ID (e.g., tracking, logging).
    ///
    /// # Example
    ///
    /// ```ignore
    /// cx.build_session_from(request)
    ///     .with_mcp_server(mcp)?
    ///     .on_proxy_session_start(request_cx, async |session_id| {
    ///         tracing::info!(%session_id, "Session started");
    ///         Ok(())
    ///     })?;
    /// // Returns immediately, session runs in background
    /// ```
    pub fn on_proxy_session_start<F, Fut>(
        self,
        request_cx: JrRequestCx<NewSessionResponse>,
        op: F,
    ) -> Result<(), crate::Error>
    where
        F: FnOnce(SessionId) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), crate::Error>> + Send,
        Link: HasPeer<ClientPeer>,
        Responder: 'static,
    {
        let connection = self.connection.clone();
        connection.spawn(async move {
            let Self {
                connection,
                request,
                dynamic_handler_registrations,
                responder,
                block_state: _,
            } = self;

            // Spawn off the connection and dynamic handlers to run indefinitely
            connection.spawn(responder.run(connection.clone()))?;
            dynamic_handler_registrations
                .into_iter()
                .for_each(|handler| handler.run_indefinitely());

            // Send the "new session" request to the agent
            let response = connection
                .send_request_to(AgentPeer, request)
                .block_task()
                .await?;

            // Extract the session-id from the response and forward
            // the restback to the client
            let session_id = response.session_id.clone();
            request_cx.respond(response)?;

            // Finally, install a dynamic handler to proxy messages from this session
            connection
                .add_dynamic_handler(ProxySessionMessages::new(session_id.clone()))?
                .run_indefinitely();

            op(session_id).await
        })
    }
}

impl<Link, Responder> SessionBuilder<Link, Responder, NonBlocking>
where
    Link: HasPeer<AgentPeer>,
    Responder: JrResponder<Link>,
{
    /// Mark this session builder as being able to block the current task.
    ///
    /// After calling this, you can use [`run_until`](Self::run_until) or
    /// [`start_session`](Self::start_session) which block the current task.
    ///
    /// This should not be used from inside a message handler like
    /// [`JrConnectionBuilder::on_receive_request`] or [`JrMessageHandler`]
    /// implementations.
    pub fn block_task(self) -> SessionBuilder<Link, Responder, Blocking> {
        SessionBuilder {
            connection: self.connection,
            request: self.request,
            dynamic_handler_registrations: self.dynamic_handler_registrations,
            responder: self.responder,
            block_state: PhantomData,
        }
    }
}

impl<Link, Responder> SessionBuilder<Link, Responder, Blocking>
where
    Link: HasPeer<AgentPeer>,
    Responder: JrResponder<Link>,
{
    /// Run this session synchronously. The current task will be blocked
    /// and `op` will be executed with the active session information.
    /// This is useful when you have MCP servers that are borrowed from your local
    /// stack frame.
    ///
    /// The `ActiveSession` passed to `op` has a non-`'static` lifetime, which
    /// prevents calling [`ActiveSession::proxy_remaining_messages`] (since the
    /// responders would terminate when `op` returns).
    ///
    /// Requires calling [`block_task`](Self::block_task) first.
    pub async fn run_until<R>(
        self,
        op: impl for<'responder> AsyncFnOnce(ActiveSession<'responder, Link>) -> Result<R, crate::Error>,
    ) -> Result<R, crate::Error> {
        let Self {
            connection,
            request,
            dynamic_handler_registrations,
            responder,
            block_state: _,
        } = self;

        let response = connection
            .send_request_to(AgentPeer, request)
            .block_task()
            .await?;

        let active_session = connection.attach_session(response, dynamic_handler_registrations)?;

        run_until(responder.run(connection.clone()), op(active_session)).await
    }

    /// Send the request to create the session and return a handle.
    /// This is an alternative to [`Self::run_until`] that avoids rightward
    /// drift but at the cost of requiring MCP servers that are `Send` and
    /// don't access data from the surrounding scope.
    ///
    /// Returns an `ActiveSession<'static, _>` because responders are spawned
    /// into background tasks that live for the connection lifetime.
    ///
    /// Requires calling [`block_task`](Self::block_task) first.
    pub async fn start_session(self) -> Result<ActiveSession<'static, Link>, crate::Error>
    where
        Responder: 'static,
    {
        let Self {
            connection,
            request,
            dynamic_handler_registrations,
            responder,
            block_state: _,
        } = self;

        let (active_session_tx, active_session_rx) = oneshot::channel();

        connection.clone().spawn(async move {
            let response = connection
                .send_request_to(AgentPeer, request)
                .block_task()
                .await?;

            connection.spawn(responder.run(connection.clone()))?;

            let active_session =
                connection.attach_session(response, dynamic_handler_registrations)?;

            active_session_tx
                .send(active_session)
                .map_err(|_| crate::Error::internal_error())?;

            Ok(())
        })?;

        active_session_rx
            .await
            .map_err(|_| crate::Error::internal_error())
    }

    /// Start a session and proxy all messages between client and agent.
    ///
    /// This is a convenience method that combines [`start_session`](Self::start_session),
    /// responding to the client, and [`ActiveSession::proxy_remaining_messages`].
    /// Use this when you want to inject MCP servers into a session but don't need
    /// to actively interact with it.
    ///
    /// For more control (e.g., to send some messages before proxying), use
    /// [`start_session`](Self::start_session) instead and call
    /// [`proxy_remaining_messages`](ActiveSession::proxy_remaining_messages) manually.
    ///
    /// Requires calling [`block_task`](Self::block_task) first.
    pub async fn start_session_proxy(
        self,
        request_cx: JrRequestCx<NewSessionResponse>,
    ) -> Result<SessionId, crate::Error>
    where
        Link: HasPeer<ClientPeer>,
        Responder: 'static,
    {
        let active_session = self.start_session().await?;
        let session_id = active_session.session_id().clone();
        request_cx.respond(active_session.response())?;
        active_session.proxy_remaining_messages()?;
        Ok(session_id)
    }
}

/// Active session struct that lets you send prompts and receive updates.
///
/// The `'responder` lifetime represents the span during which responders
/// (e.g., MCP server handlers) are active. When created via [`SessionBuilder::start_session`],
/// this is `'static` because responders are spawned into background tasks.
/// When created via [`SessionBuilder::run_until`], this is tied to the
/// closure scope, preventing [`Self::proxy_remaining_messages`] from being called
/// (since the responders would die when the closure returns).
pub struct ActiveSession<'responder, Link>
where
    Link: HasPeer<AgentPeer>,
{
    session_id: SessionId,
    update_rx: mpsc::UnboundedReceiver<SessionMessage>,
    update_tx: mpsc::UnboundedSender<SessionMessage>,
    modes: Option<SessionModeState>,
    meta: Option<serde_json::Value>,
    connection: JrConnectionCx<Link>,

    /// Registration for the handler that routes session messages to `update_rx`.
    /// This is separate from MCP handlers so it can be dropped independently
    /// when switching to proxy mode.
    session_handler_registration: DynamicHandlerRegistration<Link>,

    /// Registrations for MCP server handlers.
    /// These will be dropped once the active-session struct is dropped
    /// which will cause them to be deregistered.
    mcp_handler_registrations: Vec<DynamicHandlerRegistration<Link>>,

    /// Phantom lifetime representing the responder lifetime.
    _responder: PhantomData<&'responder ()>,
}

/// Incoming message from the agent
#[non_exhaustive]
#[derive(Debug)]
pub enum SessionMessage {
    /// Periodic updates with new content, tool requests, etc.
    /// Use [`MatchMessage`] to match on the message type.
    SessionMessage(MessageCx),

    /// When a prompt completes, the stop reason.
    StopReason(StopReason),
}

impl<'responder, Link> ActiveSession<'responder, Link>
where
    Link: HasPeer<AgentPeer>,
{
    /// Access the session ID.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Access modes available in this session.
    pub fn modes(&self) -> &Option<SessionModeState> {
        &self.modes
    }

    /// Access meta data from session response.
    pub fn meta(&self) -> &Option<serde_json::Value> {
        &self.meta
    }

    /// Build a `NewSessionResponse` from the session information.
    ///
    /// Useful when you need to forward the session response to a client
    /// after doing some processing.
    pub fn response(&self) -> NewSessionResponse {
        NewSessionResponse {
            session_id: self.session_id.clone(),
            modes: self.modes.clone(),
            meta: self.meta.clone(),
        }
    }

    /// Access the underlying connection context used to communicate with the agent.
    pub fn connection_cx(&self) -> JrConnectionCx<Link> {
        self.connection.clone()
    }

    /// Send a prompt to the agent. You can then read messages sent in response.
    pub fn send_prompt(&mut self, prompt: impl ToString) -> Result<(), crate::Error> {
        let update_tx = self.update_tx.clone();
        self.connection
            .send_request_to(
                AgentPeer,
                PromptRequest {
                    session_id: self.session_id.clone(),
                    prompt: vec![prompt.to_string().into()],
                    meta: None,
                },
            )
            .on_receiving_result(async move |result| {
                let PromptResponse {
                    stop_reason,
                    meta: _,
                } = result?;

                update_tx
                    .unbounded_send(SessionMessage::StopReason(stop_reason))
                    .map_err(crate::util::internal_error)?;

                Ok(())
            })
    }

    /// Read an update from the agent in response to the prompt.
    pub async fn read_update(&mut self) -> Result<SessionMessage, crate::Error> {
        use futures::StreamExt;
        let message =
            self.update_rx.next().await.ok_or_else(|| {
                crate::util::internal_error("session channel closed unexpectedly")
            })?;

        Ok(message)
    }

    /// Read all updates until the end of the turn and create a string.
    /// Ignores non-text updates.
    pub async fn read_to_string(&mut self) -> Result<String, crate::Error> {
        let mut output = String::new();
        loop {
            let update = self.read_update().await?;
            tracing::trace!(?update, "read_to_string update");
            match update {
                SessionMessage::SessionMessage(message_cx) => MatchMessage::new(message_cx)
                    .if_notification(async |notif: SessionNotification| match notif.update {
                        SessionUpdate::AgentMessageChunk(ContentChunk {
                            content: ContentBlock::Text(text),
                            meta: _,
                        }) => {
                            output.push_str(&text.text);
                            Ok(())
                        }
                        _ => Ok(()),
                    })
                    .await
                    .otherwise_ignore()?,
                SessionMessage::StopReason(_stop_reason) => break,
            }
        }
        Ok(output)
    }
}

impl<Link> ActiveSession<'static, Link>
where
    Link: HasPeer<AgentPeer>,
{
    /// Proxy all remaining messages for this session between client and agent.
    ///
    /// Use this when you want to inject MCP servers into a session but don't need
    /// to actively interact with it after setup. The session messages will be proxied
    /// between client and agent automatically.
    ///
    /// This consumes the `ActiveSession` since you're giving up active control.
    ///
    /// This method is only available on `ActiveSession<'static, _>` (from
    /// [`SessionBuilder::start_session`]) because it requires responders to
    /// outlive the method call.
    ///
    /// # Message Ordering Guarantees
    ///
    /// This method ensures proper handoff from active session mode to proxy mode
    /// without losing or reordering messages:
    ///
    /// 1. **Stop the session handler** - Drop the registration that routes messages
    ///    to `update_rx`. After this, no new messages will be queued.
    /// 2. **Close the channel** - Drop `update_tx` so we can detect when the channel
    ///    is fully drained.
    /// 3. **Drain queued messages** - Forward any messages that were already queued
    ///    in `update_rx` to the client, preserving order.
    /// 4. **Install proxy handler** - Now that all queued messages are forwarded,
    ///    install the proxy handler to handle future messages.
    ///
    /// This sequence prevents the race condition where messages could be delivered
    /// out of order or lost during the transition.
    pub fn proxy_remaining_messages(self) -> Result<(), crate::Error>
    where
        Link: HasPeer<ClientPeer>,
    {
        // Destructure self to get ownership of all fields
        let ActiveSession {
            session_id,
            mut update_rx,
            update_tx,
            connection,
            session_handler_registration,
            mcp_handler_registrations,
            // These fields are not needed for proxying
            modes: _,
            meta: _,
            _responder,
        } = self;

        // Step 1: Drop the session handler registration.
        // This unregisters the handler that was routing messages to update_rx.
        // After this point, no new messages will be added to the channel.
        drop(session_handler_registration);

        // Step 2: Drop the sender side of the channel.
        // This allows us to detect when the channel is fully drained
        // (recv will return None when empty and sender is dropped).
        drop(update_tx);

        // Step 3: Drain any messages that were already queued and forward to client.
        // These messages arrived before we dropped the handler but haven't been
        // consumed yet. We must forward them to maintain message ordering.
        while let Some(message) = update_rx.try_next().ok().flatten() {
            match message {
                SessionMessage::SessionMessage(message_cx) => {
                    // Forward the message to the client
                    connection.send_proxied_message_to(ClientPeer, message_cx)?;
                }
                SessionMessage::StopReason(_) => {
                    // StopReason is internal bookkeeping, not forwarded
                }
            }
        }

        // Step 4: Install the proxy handler for future messages.
        // Now that all queued messages have been forwarded, the proxy handler
        // can take over. Any new messages will go directly through the proxy.
        connection
            .add_dynamic_handler(ProxySessionMessages::new(session_id))?
            .run_indefinitely();

        // Keep MCP server handlers alive for the lifetime of the proxy
        for registration in mcp_handler_registrations {
            registration.run_indefinitely();
        }

        Ok(())
    }
}

struct ActiveSessionHandler<Link>
where
    Link: HasPeer<AgentPeer>,
{
    #[expect(dead_code)]
    role: Link,
    session_id: SessionId,
    update_tx: mpsc::UnboundedSender<SessionMessage>,
}

impl<Link> ActiveSessionHandler<Link>
where
    Link: HasPeer<AgentPeer>,
{
    pub fn new(
        role: Link,
        session_id: SessionId,
        update_tx: mpsc::UnboundedSender<SessionMessage>,
    ) -> Self {
        Self {
            role,
            session_id,
            update_tx,
        }
    }
}

impl<Link> JrMessageHandler for ActiveSessionHandler<Link>
where
    Link: HasPeer<AgentPeer>,
{
    type Link = Link;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Self::Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        // If this is a message for our session, grab it.
        tracing::trace!(
            ?message,
            handler_session_id = ?self.session_id,
            "ActiveSessionHandler::handle_message"
        );
        MatchMessageFrom::new(message, &cx)
            .if_message_from(AgentPeer, async |message| {
                if let Some(session_id) = message.get_session_id()? {
                    tracing::trace!(
                        message_session_id = ?session_id,
                        handler_session_id = ?self.session_id,
                        "ActiveSessionHandler::handle_message"
                    );
                    if session_id == self.session_id {
                        self.update_tx
                            .unbounded_send(SessionMessage::SessionMessage(message))
                            .map_err(crate::util::internal_error)?;
                        return Ok(Handled::Yes);
                    }
                }

                // Otherwise, pass it through.
                Ok(Handled::No {
                    message,
                    retry: false,
                })
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!("ActiveSessionHandler({})", self.session_id)
    }
}
