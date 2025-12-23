use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use fxhash::FxHashMap;

use crate::mcp::{McpClientToServer, McpServerEnd};
use crate::mcp_server::{McpContext, McpServerConnect};
use crate::schema::{
    McpConnectRequest, McpConnectResponse, McpDisconnectNotification, McpOverAcpMessage,
};
use crate::util::MatchMessageFrom;
use crate::{
    Agent, Channel, Component, Handled, HasPeer, JrConnectionCx, JrLink, JrMessageHandler,
    JrRequestCx, MessageCx, UntypedMessage,
};
use std::sync::Arc;

/// The message handler for an MCP server offered to a particular session.
/// This is added as a 'dynamic' handler to the connection context
/// (see [`JrConnectionCx::add_dynamic_handler`]) and handles MCP-over-ACP messages
/// with the appropriate ACP url.
pub(super) struct McpActiveSession<Link> {
    /// The role of the server
    #[expect(dead_code)]
    role: Link,

    /// The ACP URL created for this session
    acp_url: String,

    /// The MCP server we are managing
    mcp_connect: Arc<dyn McpServerConnect<Link>>,

    /// Active connections to MCP server tasks
    connections: FxHashMap<String, mpsc::Sender<MessageCx>>,
}

impl<Link: JrLink> McpActiveSession<Link>
where
    Link: HasPeer<Agent>,
{
    pub fn new(role: Link, acp_url: String, mcp_connect: Arc<dyn McpServerConnect<Link>>) -> Self {
        Self {
            role,
            acp_url,
            mcp_connect,
            connections: FxHashMap::default(),
        }
    }

    /// Handle connection requests for our MCP server by creating a new connection.
    /// A *connection* is an actual running instance of this MCP server.
    async fn handle_connect_request(
        &mut self,
        request: McpConnectRequest,
        request_cx: JrRequestCx<McpConnectResponse>,
        outer_cx: &JrConnectionCx<Link>,
    ) -> Result<Handled<(McpConnectRequest, JrRequestCx<McpConnectResponse>)>, crate::Error> {
        // Check that this is for our MCP server
        if request.acp_url != self.acp_url {
            return Ok(Handled::No {
                message: (request, request_cx),
                retry: false,
            });
        }

        // Create a unique connection ID and a channel for future communication
        let connection_id = format!("mcp-over-acp-connection:{}", uuid::Uuid::new_v4());
        let (mcp_server_tx, mut mcp_server_rx) = mpsc::channel(128);
        self.connections
            .insert(connection_id.clone(), mcp_server_tx);

        // Create connected channel pair for client-server communication
        let (client_channel, server_channel) = Channel::duplex();

        // Create client-side handler that wraps messages and forwards to successor
        let client_component = {
            let connection_id = connection_id.clone();
            let outer_cx = outer_cx.clone();

            McpClientToServer::builder()
                .on_receive_message(
                    async move |message: MessageCx, _mcp_cx| {
                        // Wrap the message in McpOverAcp{Request,Notification} and forward to successor
                        let wrapped = message.map(
                            |request, request_cx| {
                                (
                                    McpOverAcpMessage {
                                        connection_id: connection_id.clone(),
                                        message: request,
                                        meta: None,
                                    },
                                    request_cx,
                                )
                            },
                            |notification| McpOverAcpMessage {
                                connection_id: connection_id.clone(),
                                message: notification,
                                meta: None,
                            },
                        );
                        outer_cx.send_proxied_message_to(Agent, wrapped)
                    },
                    crate::on_receive_message!(),
                )
                .with_spawned(move |mcp_cx| async move {
                    // Messages we pull off this channel were sent from the agent.
                    // Forward them back to the MCP server.
                    while let Some(msg) = mcp_server_rx.next().await {
                        mcp_cx.send_proxied_message_to(McpServerEnd, msg)?;
                    }
                    Ok(())
                })
        };

        // Get the MCP server component
        let spawned_server = self.mcp_connect.connect(McpContext {
            acp_url: request.acp_url.clone(),
            connection_cx: outer_cx.clone(),
        });

        // Spawn both sides of the connection
        let spawn_results = outer_cx
            .spawn(async move { client_component.serve(client_channel).await })
            .and_then(|()| {
                // Spawn the MCP server serving the server channel
                outer_cx.spawn(async move { spawned_server.serve(server_channel).await })
            });

        match spawn_results {
            Ok(()) => {
                request_cx.respond(McpConnectResponse {
                    connection_id,
                    meta: None,
                })?;
                Ok(Handled::Yes)
            }

            Err(err) => {
                request_cx.respond_with_error(err)?;
                Ok(Handled::Yes)
            }
        }
    }

    /// Forward MCP-over-ACP requests to the connection.
    async fn handle_mcp_over_acp_request(
        &mut self,
        request: McpOverAcpMessage<UntypedMessage>,
        request_cx: JrRequestCx<serde_json::Value>,
    ) -> Result<
        Handled<(
            McpOverAcpMessage<UntypedMessage>,
            JrRequestCx<serde_json::Value>,
        )>,
        crate::Error,
    > {
        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(mcp_server_tx) = self.connections.get_mut(&request.connection_id) else {
            return Ok(Handled::No {
                message: (request, request_cx),
                retry: false,
            });
        };

        mcp_server_tx
            .send(MessageCx::Request(request.message, request_cx))
            .await
            .map_err(crate::Error::into_internal_error)?;

        Ok(Handled::Yes)
    }

    /// Forward MCP-over-ACP notifications to the connection.
    async fn handle_mcp_over_acp_notification(
        &mut self,
        notification: McpOverAcpMessage<UntypedMessage>,
    ) -> Result<Handled<McpOverAcpMessage<UntypedMessage>>, crate::Error> {
        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(mcp_server_tx) = self.connections.get_mut(&notification.connection_id) else {
            return Ok(Handled::No {
                message: notification,
                retry: false,
            });
        };

        mcp_server_tx
            .send(MessageCx::Notification(notification.message))
            .await
            .map_err(crate::Error::into_internal_error)?;

        Ok(Handled::Yes)
    }

    /// Disconnect a connection.
    async fn handle_mcp_disconnect_notification(
        &mut self,
        successor_notification: McpDisconnectNotification,
    ) -> Result<Handled<McpDisconnectNotification>, crate::Error> {
        // Remove connection if we have it. Otherwise, do not handle the notification.
        if let Some(_) = self
            .connections
            .remove(&successor_notification.connection_id)
        {
            Ok(Handled::Yes)
        } else {
            Ok(Handled::No {
                message: successor_notification,
                retry: false,
            })
        }
    }
}

impl<Link: JrLink> JrMessageHandler for McpActiveSession<Link>
where
    Link: HasPeer<Agent>,
{
    type Link = Link;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "McpServerSession"
    }

    async fn handle_message(
        &mut self,
        message: MessageCx,
        connection_cx: JrConnectionCx<Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        MatchMessageFrom::new(message, &connection_cx)
            // MCP connect requests come from the Agent direction (wrapped in SuccessorMessage)
            .if_request_from(Agent, async |request, request_cx| {
                self.handle_connect_request(request, request_cx, &connection_cx)
                    .await
            })
            .await
            // MCP over ACP requests come from the Agent direction
            .if_request_from(Agent, async |request, request_cx| {
                self.handle_mcp_over_acp_request(request, request_cx).await
            })
            .await
            // MCP over ACP notifications come from the Agent direction
            .if_notification_from(Agent, async |notification| {
                self.handle_mcp_over_acp_notification(notification).await
            })
            .await
            // MCP disconnect notifications come from the Agent direction
            .if_notification_from(Agent, async |notification| {
                self.handle_mcp_disconnect_notification(notification).await
            })
            .await
            .done()
    }
}
