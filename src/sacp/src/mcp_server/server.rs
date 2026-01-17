//! MCP server builder for creating MCP servers.

use std::sync::Arc;

use agent_client_protocol_schema::NewSessionRequest;
use futures::{StreamExt, channel::mpsc};
use uuid::Uuid;

use crate::{
    AgentPeer, ClientPeer, Component, DynComponent, Handled, HasPeer, JrConnectionCx, JrLink,
    JsonRpcMessageHandler, MessageCx,
    jsonrpc::{
        DynamicHandlerRegistration,
        responder::{JrResponder, NullResponder},
    },
    mcp::{McpClientPeer, McpClientToServer, McpServerPeer, McpServerToClient},
    mcp_server::{
        McpContext, McpServerConnect, active_session::McpActiveSession, builder::McpServerBuilder,
    },
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
    /// The ACP URL we assigned for this mcp server; always unique
    acp_url: String,

    /// The "connect" instance
    connect: Arc<dyn McpServerConnect<Link>>,

    /// The "responder" is a task that should be run alongside the message handler.
    /// Some futures direct messages back through channels to this future which actually
    /// handles responding to the client.
    ///
    /// This is how we bridge the gap between the rmcp implementation,
    /// which requires `'static`, and our APIs, which do not.
    responder: Responder,
}

impl<Link: JrLink> McpServer<Link, NullResponder> {
    /// Create an empty server with no content.
    pub fn builder(name: impl ToString) -> McpServerBuilder<Link, NullResponder> {
        McpServerBuilder::new(name.to_string())
    }
}

impl<Link: JrLink, Responder: JrResponder<Link>> McpServer<Link, Responder> {
    /// Create an MCP server from something that implements the [`McpServerConnect`](`super::McpServerConnect`) trait.
    ///
    /// # See also
    ///
    /// See [`Self::builder`] to construct MCP servers from Rust code.
    pub fn new(c: impl McpServerConnect<Link>, responder: Responder) -> Self {
        McpServer {
            acp_url: format!("acp:{}", Uuid::new_v4()),
            connect: Arc::new(c),
            responder,
        }
    }

    /// Split this MCP server into the message handler and a future that must be run while the handler is active.
    pub(crate) fn into_handler_and_responder(self) -> (McpNewSessionHandler<Link>, Responder)
    where
        Link: HasPeer<AgentPeer>,
    {
        let Self {
            acp_url,
            connect,
            responder,
        } = self;
        (McpNewSessionHandler::new(acp_url, connect), responder)
    }
}

/// Message handler created from a [`McpServer`].
pub(crate) struct McpNewSessionHandler<Link>
where
    Link: HasPeer<AgentPeer>,
{
    acp_url: String,
    connect: Arc<dyn McpServerConnect<Link>>,
    active_session: McpActiveSession<Link>,
}

impl<Link: JrLink> McpNewSessionHandler<Link>
where
    Link: HasPeer<AgentPeer>,
{
    pub fn new(acp_url: String, connect: Arc<dyn McpServerConnect<Link>>) -> Self {
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

    /// Modify the new session request to include this MCP server.
    fn modify_new_session_request(&self, request: &mut NewSessionRequest) {
        request.mcp_servers.push(crate::schema::McpServer::Http(
            crate::schema::McpServerHttp::new(self.connect.name(), self.acp_url.clone()),
        ));
    }
}

impl<Link: JrLink> McpNewSessionHandler<Link>
where
    Link: HasPeer<AgentPeer>,
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
    pub fn into_dynamic_handler(
        self,
        request: &mut NewSessionRequest,
        cx: &JrConnectionCx<Link>,
    ) -> Result<DynamicHandlerRegistration<Link>, crate::Error>
    where
        Link: HasPeer<AgentPeer>,
    {
        self.modify_new_session_request(request);
        cx.add_dynamic_handler(self.active_session)
    }
}

impl<Link: JrLink> JsonRpcMessageHandler for McpNewSessionHandler<Link>
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

impl<R> Component<McpServerToClient> for McpServer<McpServerToClient, R>
where
    R: JrResponder<McpServerToClient> + 'static,
{
    async fn serve(self, client: impl Component<McpClientToServer>) -> Result<(), crate::Error> {
        let Self {
            acp_url,
            connect,
            responder,
        } = self;

        let (tx, mut rx) = mpsc::unbounded();

        McpServerToClient::builder()
            .with_responder(responder)
            .on_receive_message(
                async |message_from_client: MessageCx, _cx| {
                    tx.unbounded_send(message_from_client)
                        .map_err(|_| crate::util::internal_error("nobody listening to mcp server"))
                },
                crate::on_receive_message!(),
            )
            .with_spawned(async move |server_to_client_cx| {
                let spawned_server: DynComponent<McpServerToClient> = connect.connect(McpContext {
                    acp_url,
                    connection_cx: server_to_client_cx.clone(),
                });

                McpClientToServer::builder()
                    .on_receive_message(
                        async |message_from_server: MessageCx, _client_to_server_cx| {
                            // when we receive a message from the server, fwd to the client
                            server_to_client_cx
                                .send_proxied_message_to(McpClientPeer, message_from_server)
                        },
                        crate::on_receive_message!(),
                    )
                    .connect_to(spawned_server)?
                    .run_until(async |client_to_server_cx| {
                        while let Some(message_from_client) = rx.next().await {
                            client_to_server_cx
                                .send_proxied_message_to(McpServerPeer, message_from_client)?;
                        }
                        Ok(())
                    })
                    .await
            })
            .serve(client)
            .await
    }
}
