use std::path::Path;

use agent_client_protocol_schema::{
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionModeState,
    StopReason,
};
use futures::channel::mpsc;

use crate::{
    Agent, Handled, HasEndpoint, JrConnectionCx, JrMessageHandlerSend, JrRole, MessageCx,
    jsonrpc::DynamicHandlerRegistration, mcp_server::McpServiceRegistry, schema::SessionId,
};

impl<Role: JrRole> JrConnectionCx<Role>
where
    Role: HasEndpoint<Agent>,
{
    /// Session builder for a new session request.
    pub fn build_session(&self, cwd: impl AsRef<Path>) -> SessionBuilder<Role> {
        SessionBuilder::new(
            self,
            NewSessionRequest {
                cwd: cwd.as_ref().to_owned(),
                mcp_servers: Default::default(),
                meta: Default::default(),
            },
        )
    }

    /// Given a session response received from the agent,
    /// attach a handler to process messages related to this session
    /// and let you access them.
    ///
    /// Normally you would not use this method directly but would
    /// instead use [`Self::build_session`] and then [`SessionBuilder::send_request`].
    ///
    /// The vector `dynamic_handler_registrations` contains any dynamic
    /// handle registrations associated with this session (e.g., from MCP servers).
    /// You can simply pass `Default::default()` if not applicable.
    pub fn attach_session(
        &self,
        response: NewSessionResponse,
        mut dynamic_handler_registrations: Vec<DynamicHandlerRegistration<Role>>,
    ) -> Result<ActiveSession<Role>, crate::Error> {
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
        })
    }
}

/// Session builder for a new session request.
/// Allows you to add MCP servers or set other details for this session.
#[must_use = "use `send_request` to send the request"]
pub struct SessionBuilder<Role>
where
    Role: HasEndpoint<Agent>,
{
    connection: JrConnectionCx<Role>,
    request: NewSessionRequest,
    dynamic_handler_registrations: Vec<DynamicHandlerRegistration<Role>>,
}

impl<Role> SessionBuilder<Role>
where
    Role: HasEndpoint<Agent>,
{
    fn new(connection: &JrConnectionCx<Role>, request: NewSessionRequest) -> Self {
        SessionBuilder {
            connection: connection.clone(),
            request,
            dynamic_handler_registrations: Default::default(),
        }
    }

    /// Add the MCP servers from the given registry to this session.
    pub fn with_mcp_servers(
        mut self,
        mcp_server: &McpServiceRegistry<Role>,
    ) -> Result<Self, crate::Error> {
        self.dynamic_handler_registrations
            .extend(mcp_server.add_registered_mcp_servers_to(&mut self.request, &self.connection)?);
        Ok(self)
    }

    /// Send the request to create the session.
    pub async fn send_request(self) -> Result<ActiveSession<Role>, crate::Error> {
        let response = self
            .connection
            .send_request_to(Agent, self.request)
            .block_task()
            .await?;
        self.connection
            .attach_session(response, self.dynamic_handler_registrations)
    }
}

/// Active session struct that lets you send prompts and receive updates.
pub struct ActiveSession<Role>
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
    #[expect(dead_code)]
    dynamic_handler_registrations: Vec<DynamicHandlerRegistration<Role>>,
}

/// Incoming message from the agent
#[non_exhaustive]
pub enum SessionMessage {
    /// Periodic updates with new content, tool requests, etc.
    /// Use [`MatchMessage`] to match on the message type.
    SessionMessage(MessageCx),

    /// When a prompt completes, the stop reason.
    StopReason(StopReason),
}

impl<R> ActiveSession<R>
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
            .await_when_result_received(async move |result| {
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

impl<Role> JrMessageHandlerSend for ActiveSessionHandler<Role>
where
    Role: HasEndpoint<Agent>,
{
    type Role = Role;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        _cx: JrConnectionCx<Self::Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        // If this is a message for our session, grab it.
        tracing::trace!(
            ?message,
            handler_session_id = ?self.session_id,
            "ActiveSessionHandler::handle_message"
        );
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
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!("ActiveSessionHandler({})", self.session_id)
    }
}
