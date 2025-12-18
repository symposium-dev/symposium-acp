//! MCP server builder for creating MCP servers.

use std::sync::Arc;

use agent_client_protocol_schema::NewSessionRequest;
use uuid::Uuid;

use crate::{
    Agent, Client, Handled, HasEndpoint, JrConnectionCx, JrMessageHandlerSend, JrRole,
    jsonrpc::{
        DynamicHandlerRegistration,
        responder::{JrResponder, NullResponder},
    },
    mcp_server::{McpServerConnect, active_session::McpActiveSession, builder::McpServerBuilder},
    util::MatchMessageFrom,
};

/// An MCP server that can be attached to ACP connections.
///
/// `McpServer` wraps an [`McpServerConnect`] implementation and can be used either:
/// - As a message handler via [`JrConnectionBuilder::with_handler`], automatically
///   attaching to new sessions
/// - Manually via [`Self::add_to_new_session`] for more control
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
/// Or implement [`McpServerConnect`] for custom server behavior:
///
/// ```rust,ignore
/// let server = McpServer::new(MyCustomServerConnect);
/// ```
pub struct McpServer<Role: JrRole, Responder: JrResponder<Role> = NullResponder>
where
    Role: HasEndpoint<Agent>,
{
    /// The "message handler" handles incoming messages to the MCP server (speaks the MCP protocol).
    message_handler: McpMessageHandler<Role>,

    /// The "responder" is a task that should be run alongside the message handler.
    /// Some futures direct messages back through channels to this future which actually
    /// handles responding to the client.
    ///
    /// This is how we bridge the gap between the rmcp implementation,
    /// which requires `'static`, and our APIs, which do not.
    responder: Responder,
}

impl<Role: JrRole> McpServer<Role, NullResponder>
where
    Role: HasEndpoint<Agent>,
{
    /// Create an empty server with no content.
    pub fn builder(name: impl ToString) -> McpServerBuilder<Role, NullResponder> {
        McpServerBuilder::new(name.to_string())
    }
}

impl<Role: JrRole, Responder: JrResponder<Role>> McpServer<Role, Responder>
where
    Role: HasEndpoint<Agent>,
{
    /// Create an MCP server from something that implements the [`McpServerConnect`] trait.
    ///
    /// # See also
    ///
    /// See [`Self::builder`] to construct MCP servers from Rust code.
    pub fn new(c: impl McpServerConnect<Role>, responder: Responder) -> Self {
        McpServer {
            message_handler: McpMessageHandler {
                connect: Arc::new(c),
            },
            responder,
        }
    }

    /// Split this MCP server into the message handler and a future that must be run while the handler is active.
    pub(crate) fn into_handler_and_responder(self) -> (McpMessageHandler<Role>, Responder) {
        (self.message_handler, self.responder)
    }
}

/// Message handler created from a [`McpServer`].
#[derive(Clone)]
pub struct McpMessageHandler<Role: JrRole>
where
    Role: HasEndpoint<Agent>,
{
    connect: Arc<dyn McpServerConnect<Role>>,
}

impl<Role: JrRole> McpMessageHandler<Role>
where
    Role: HasEndpoint<Agent>,
{
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
    pub fn add_to_new_session(
        &self,
        request: &mut NewSessionRequest,
        cx: &JrConnectionCx<Role>,
    ) -> Result<DynamicHandlerRegistration<Role>, crate::Error> {
        let acp_url = format!("acp:{}", Uuid::new_v4());
        let connection =
            McpActiveSession::new(Role::default(), acp_url.clone(), self.connect.clone());
        request.mcp_servers.push(crate::schema::McpServer::Http {
            name: self.connect.name(),
            url: acp_url,
            headers: Default::default(),
        });
        cx.add_dynamic_handler(connection)
    }
}

impl<Role: JrRole> JrMessageHandlerSend for McpMessageHandler<Role>
where
    Role: HasEndpoint<Client> + HasEndpoint<Agent>,
{
    type Role = Role;

    async fn handle_message(
        &mut self,
        message: crate::MessageCx,
        cx: crate::JrConnectionCx<Self::Role>,
    ) -> Result<crate::Handled<crate::MessageCx>, crate::Error> {
        MatchMessageFrom::new(message, &cx)
            .if_request_from(
                Client,
                async |mut request: NewSessionRequest, request_cx| {
                    self.add_to_new_session(&mut request, &cx)?
                        .run_indefinitely();
                    Ok(Handled::No {
                        message: (request, request_cx),
                        retry: false,
                    })
                },
            )
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!("McpServer({})", self.connect.name())
    }
}
