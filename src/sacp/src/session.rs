use std::{future::Future, marker::PhantomData, path::Path};

use agent_client_protocol_schema::{
    ContentBlock, ContentChunk, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SessionModeState, SessionNotification, SessionUpdate, StopReason,
};
use futures::channel::mpsc;
use tokio::sync::oneshot;

use crate::{
    Agent, Client, Handled, HasEndpoint, JrConnectionCx, JrMessageHandler, JrRequestCx, JrRole,
    MessageCx,
    jsonrpc::{
        DynamicHandlerRegistration,
        responder::{ChainResponder, JrResponder, NullResponder},
    },
    mcp_server::McpServer,
    role::ProxySessionMessages,
    schema::SessionId,
    util::{MatchMessage, MatchMessageFrom, run_until},
};

/// Marker type indicating the session builder will block the current task.
pub struct Blocking;

/// Marker type indicating the session builder will not block the current task.
pub struct NonBlocking;

impl<Role: JrRole> JrConnectionCx<Role>
where
    Role: HasEndpoint<Agent>,
{
    /// Session builder for a new session request.
    pub fn build_session(&self, cwd: impl AsRef<Path>) -> SessionBuilder<Role, NullResponder> {
        SessionBuilder::new(
            self,
            NewSessionRequest {
                cwd: cwd.as_ref().to_owned(),
                mcp_servers: Default::default(),
                meta: Default::default(),
            },
        )
    }

    /// Session builder starting from an existing request.
    ///
    /// Use this when you've intercepted a `session.new` request and want to
    /// modify it (e.g., inject MCP servers) before forwarding.
    pub fn build_session_from(
        &self,
        request: NewSessionRequest,
    ) -> SessionBuilder<Role, NullResponder> {
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
        mut dynamic_handler_registrations: Vec<DynamicHandlerRegistration<Role>>,
    ) -> Result<ActiveSession<'responder, Role>, crate::Error> {
        let NewSessionResponse {
            session_id,
            modes,
            meta,
        } = response;

        let (update_tx, update_rx) = mpsc::unbounded();
        let handler =
            ActiveSessionHandler::new(Role::default(), session_id.clone(), update_tx.clone());
        let registration = self.add_dynamic_handler(handler)?;
        dynamic_handler_registrations.push(registration);

        Ok(ActiveSession {
            session_id,
            modes,
            meta,
            update_rx,
            update_tx,
            connection: self.clone(),
            dynamic_handler_registrations,
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
    Role,
    Responder: JrResponder<Role> = NullResponder,
    BlockState = NonBlocking,
> where
    Role: HasEndpoint<Agent>,
{
    connection: JrConnectionCx<Role>,
    request: NewSessionRequest,
    dynamic_handler_registrations: Vec<DynamicHandlerRegistration<Role>>,
    responder: Responder,
    _block_state: PhantomData<BlockState>,
}

impl<Role> SessionBuilder<Role, NullResponder, NonBlocking>
where
    Role: HasEndpoint<Agent>,
{
    fn new(connection: &JrConnectionCx<Role>, request: NewSessionRequest) -> Self {
        SessionBuilder {
            connection: connection.clone(),
            request,
            dynamic_handler_registrations: Default::default(),
            responder: NullResponder,
            _block_state: PhantomData,
        }
    }
}

impl<Role, Responder, BlockState> SessionBuilder<Role, Responder, BlockState>
where
    Role: HasEndpoint<Agent>,
    Responder: JrResponder<Role>,
{
    /// Add the MCP servers from the given registry to this session.
    pub fn with_mcp_server<R>(
        mut self,
        mcp_server: McpServer<Role, R>,
    ) -> Result<SessionBuilder<Role, ChainResponder<Responder, R>, BlockState>, crate::Error>
    where
        R: JrResponder<Role>,
    {
        let (handler, responder) = mcp_server.into_handler_and_responder();
        self.dynamic_handler_registrations
            .push(handler.into_dynamic_handler(&mut self.request, &self.connection)?);
        Ok(SessionBuilder {
            connection: self.connection,
            request: self.request,
            dynamic_handler_registrations: self.dynamic_handler_registrations,
            responder: ChainResponder::new(self.responder, responder),
            _block_state: PhantomData,
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
        F: FnOnce(ActiveSession<'static, Role>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), crate::Error>> + Send,
    {
        let connection = self.connection.clone();
        connection.spawn(async move {
            let response = self
                .connection
                .send_request_to(Agent, self.request)
                .block_task()
                .await?;

            let cx = self.connection.clone();
            self.connection.spawn(self.responder.run(cx))?;

            let active_session = self
                .connection
                .attach_session(response, self.dynamic_handler_registrations)?;

            op(active_session).await
        })
    }
}

impl<Role, Responder> SessionBuilder<Role, Responder, NonBlocking>
where
    Role: HasEndpoint<Agent>,
    Responder: JrResponder<Role>,
{
    /// Mark this session builder as blocking.
    ///
    /// After calling this, you can use [`run_until`](Self::run_until) or
    /// [`start_session`](Self::start_session) which block the current task.
    pub fn block_task(self) -> SessionBuilder<Role, Responder, Blocking> {
        SessionBuilder {
            connection: self.connection,
            request: self.request,
            dynamic_handler_registrations: self.dynamic_handler_registrations,
            responder: self.responder,
            _block_state: PhantomData,
        }
    }
}

impl<Role, Responder> SessionBuilder<Role, Responder, Blocking>
where
    Role: HasEndpoint<Agent>,
    Responder: JrResponder<Role>,
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
        op: impl for<'responder> AsyncFnOnce(ActiveSession<'responder, Role>) -> Result<R, crate::Error>,
    ) -> Result<R, crate::Error> {
        let response = self
            .connection
            .send_request_to(Agent, self.request)
            .block_task()
            .await?;

        let active_session = self
            .connection
            .attach_session(response, self.dynamic_handler_registrations)?;

        run_until(
            self.responder.run(self.connection.clone()),
            op(active_session),
        )
        .await
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
    pub async fn start_session(self) -> Result<ActiveSession<'static, Role>, crate::Error>
    where
        Responder: 'static,
    {
        let (active_session_tx, active_session_rx) = oneshot::channel();

        let connection = self.connection.clone();
        connection.spawn(async move {
            let response = self
                .connection
                .send_request_to(Agent, self.request)
                .block_task()
                .await?;

            let cx = self.connection.clone();
            self.connection.spawn(self.responder.run(cx))?;

            let active_session = self
                .connection
                .attach_session(response, self.dynamic_handler_registrations)?;

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
    pub async fn start_session_proxy(
        self,
        request_cx: JrRequestCx<NewSessionResponse>,
    ) -> Result<(), crate::Error>
    where
        Role: HasEndpoint<Client>,
        Responder: 'static,
    {
        let active_session = self.start_session().await?;
        request_cx.respond(active_session.response())?;
        active_session.proxy_remaining_messages()
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
pub struct ActiveSession<'responder, Role>
where
    Role: HasEndpoint<Agent>,
{
    session_id: SessionId,
    update_rx: mpsc::UnboundedReceiver<SessionMessage>,
    update_tx: mpsc::UnboundedSender<SessionMessage>,
    modes: Option<SessionModeState>,
    meta: Option<serde_json::Value>,
    connection: JrConnectionCx<Role>,

    /// Collect registrations from dynamic handlers for MCP servers etc.
    /// These will be dropped once the active-session struct is dropped
    /// which will cause them to be deregistered.
    dynamic_handler_registrations: Vec<DynamicHandlerRegistration<Role>>,

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

impl<'responder, R> ActiveSession<'responder, R>
where
    R: HasEndpoint<Agent>,
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
    pub fn connection_cx(&self) -> JrConnectionCx<R> {
        self.connection.clone()
    }

    /// Send a prompt to the agent. You can then read messages sent in response.
    pub fn send_prompt(&mut self, prompt: impl ToString) -> Result<(), crate::Error> {
        let update_tx = self.update_tx.clone();
        self.connection
            .send_request_to(
                Agent,
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

impl<R> ActiveSession<'static, R>
where
    R: HasEndpoint<Agent>,
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
    pub fn proxy_remaining_messages(self) -> Result<(), crate::Error>
    where
        R: HasEndpoint<Client>,
    {
        // Add dynamic handler to proxy session messages
        self.connection
            .add_dynamic_handler(ProxySessionMessages::new(self.session_id))?
            .run_indefinitely();

        // Keep MCP server handlers alive
        for registration in self.dynamic_handler_registrations {
            registration.run_indefinitely();
        }

        Ok(())
    }
}

struct ActiveSessionHandler<Role>
where
    Role: HasEndpoint<Agent>,
{
    #[expect(dead_code)]
    role: Role,
    session_id: SessionId,
    update_tx: mpsc::UnboundedSender<SessionMessage>,
}

impl<Role> ActiveSessionHandler<Role>
where
    Role: HasEndpoint<Agent>,
{
    pub fn new(
        role: Role,
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

impl<Role> JrMessageHandler for ActiveSessionHandler<Role>
where
    Role: HasEndpoint<Agent>,
{
    type Role = Role;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Self::Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        // If this is a message for our session, grab it.
        tracing::trace!(
            ?message,
            handler_session_id = ?self.session_id,
            "ActiveSessionHandler::handle_message"
        );
        MatchMessageFrom::new(message, &cx)
            .if_message_from(Agent, async |message| {
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
