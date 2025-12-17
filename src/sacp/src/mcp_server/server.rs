//! MCP server builder for creating MCP servers.

use std::sync::Arc;

use agent_client_protocol_schema::NewSessionRequest;
use uuid::Uuid;

use crate::{
    Agent, Client, Handled, HasEndpoint, JrConnectionCx, JrMessageHandler, JrRole,
    jsonrpc::DynamicHandlerRegistration,
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
pub struct McpServer<Role: JrRole> {
    connect: Arc<dyn McpServerConnect<Role>>,
}

impl<Role: JrRole> McpServer<Role>
where
    Role: HasEndpoint<Agent>,
{
    /// Create an empty server with no content.
    pub fn builder(name: impl ToString) -> McpServerBuilder<Role> {
        McpServerBuilder::new(name.to_string())
    }

    /// Create an MCP server from something that implements the [`McpServerConnect`] trait.
    ///
    /// # See also
    ///
    /// See [`Self::builder`] to construct MCP servers from Rust code.
    pub fn new(c: impl McpServerConnect<Role>) -> Self {
        McpServer {
            connect: Arc::new(c),
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

impl<Role: JrRole> JrMessageHandler for McpServer<Role>
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
