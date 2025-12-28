//! MCP server builder for creating MCP servers.

use std::sync::Arc;

use agent_client_protocol_schema::NewSessionRequest;
use uuid::Uuid;

use crate::{
    AgentPeer, ClientPeer, Handled, HasPeer, JrConnectionCx, JrLink, JrMessageHandler,
    jsonrpc::{
        DynamicHandlerRegistration,
        responder::{JrResponder, NullResponder},
    },
    mcp_server::{McpServerConnect, active_session::McpActiveSession, builder::McpServerBuilder},
    util::MatchMessageFrom,
};

/// An MCP server that can be attached to ACP connections.
///
/// `McpServer` wraps an [`McpServerConnect`](`super::McpServerConnect`) implementation and can be used either:
/// - As a message handler via [`JrConnectionBuilder::with_handler`](`crate::JrConnectionBuilder::with_handler`), automatically
///   attaching to new sessions
/// - Manually for more control
///
/// # Creating an MCP Server
///
/// Use [`McpServer::builder`] to create a server with tools:
///
/// ```rust,ignore
/// let server = McpServer::builder("my-server".to_string())
///     .instructions("A helpful assistant")
///     .tool(MyTool)
///     .build();
/// ```
///
/// Or implement [`McpServerConnect`](`super::McpServerConnect`) for custom server behavior:
///
/// ```rust,ignore
/// let server = McpServer::new(MyCustomServerConnect);
/// ```
pub struct McpServer<Link, Responder = NullResponder> {
    /// The "message handler" handles incoming messages to the MCP server (speaks the MCP protocol).
    message_handler: McpNewSessionHandler<Link>,

    /// The "responder" is a task that should be run alongside the message handler.
    /// Some futures direct messages back through channels to this future which actually
    /// handles responding to the client.
    ///
    /// This is how we bridge the gap between the rmcp implementation,
    /// which requires `'static`, and our APIs, which do not.
    responder: Responder,
}

impl<Link: JrLink> McpServer<Link, NullResponder>
where
    Link: HasPeer<AgentPeer>,
{
    /// Create an empty server with no content.
    pub fn builder(name: impl ToString) -> McpServerBuilder<Link, NullResponder> {
        McpServerBuilder::new(name.to_string())
    }
}

impl<Link: JrLink, Responder: JrResponder<Link>> McpServer<Link, Responder>
where
    Link: HasPeer<AgentPeer>,
{
    /// Create an MCP server from something that implements the [`McpServerConnect`](`super::McpServerConnect`) trait.
    ///
    /// # See also
    ///
    /// See [`Self::builder`] to construct MCP servers from Rust code.
    pub fn new(c: impl McpServerConnect<Link>, responder: Responder) -> Self {
        McpServer {
            message_handler: McpNewSessionHandler::new(c),
            responder,
        }
    }

    /// Split this MCP server into the message handler and a future that must be run while the handler is active.
    pub(crate) fn into_handler_and_responder(self) -> (McpNewSessionHandler<Link>, Responder) {
        (self.message_handler, self.responder)
    }
}

/// Message handler created from a [`McpServer`].
pub(crate) struct McpNewSessionHandler<Link> {
    acp_url: String,
    connect: Arc<dyn McpServerConnect<Link>>,
    active_session: McpActiveSession<Link>,
}

impl<Link: JrLink> McpNewSessionHandler<Link>
where
    Link: HasPeer<AgentPeer>,
{
    pub fn new(c: impl McpServerConnect<Link>) -> Self {
        let acp_url = format!("acp:{}", Uuid::new_v4());
        let connect = Arc::new(c);
        Self {
            active_session: McpActiveSession::new(
                Link::default(),
                acp_url.clone(),
                connect.clone(),
            ),
            acp_url,
            connect,
        }
    }

    /// Attach this server to the new session, spawning off a dynamic handler that will
    /// manage requests coming from this session.
    ///
    /// # Return value
    ///
    /// Returns a [`DynamicHandlerRegistration`] for the handler that intercepts messages
    /// related to this MCP server. Once the value is dropped, the MCP server messages
    /// will no longer be received, so you need to keep this value alive as long as the session
    /// is in use. You can also invoke [`DynamicHandlerRegistration::run_indefinitely`]
    /// if you want to keep the handler running indefinitely.
    pub fn into_dynamic_handler(
        self,
        request: &mut NewSessionRequest,
        cx: &JrConnectionCx<Link>,
    ) -> Result<DynamicHandlerRegistration<Link>, crate::Error> {
        self.modify_new_session_request(request);
        cx.add_dynamic_handler(self.active_session)
    }

    /// Modify the new session request to include this MCP server.
    fn modify_new_session_request(&self, request: &mut NewSessionRequest) {
        request.mcp_servers.push(crate::schema::McpServer::Http {
            name: self.connect.name(),
            url: self.acp_url.clone(),
            headers: Default::default(),
        });
    }
}

impl<Link: JrLink> JrMessageHandler for McpNewSessionHandler<Link>
where
    Link: HasPeer<ClientPeer> + HasPeer<AgentPeer>,
{
    type Link = Link;

    async fn handle_message(
        &mut self,
        message: crate::MessageCx,
        cx: crate::JrConnectionCx<Self::Link>,
    ) -> Result<crate::Handled<crate::MessageCx>, crate::Error> {
        MatchMessageFrom::new(message, &cx)
            .if_request_from(
                ClientPeer,
                async |mut request: NewSessionRequest, request_cx| {
                    self.modify_new_session_request(&mut request);
                    Ok(Handled::No {
                        message: (request, request_cx),
                        retry: false,
                    })
                },
            )
            .await
            .otherwise_delegate(&mut self.active_session)
            .await
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!("McpServer({})", self.connect.name())
    }
}
