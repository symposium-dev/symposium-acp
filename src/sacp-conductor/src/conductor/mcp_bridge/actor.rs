use futures::{SinkExt as _, StreamExt as _, channel::mpsc};
use sacp::{Component, DynComponent, JrHandlerChain, MessageAndCx};
use sacp_proxy::McpDisconnectNotification;
use tracing::info;

use crate::conductor::ConductorMessage;

/// Trait for actors that handle MCP bridge connections.
///
/// Implementations bridge between MCP clients and the conductor's ACP message flow.
#[derive(Debug)]
pub struct McpBridgeConnectionActor {
    /// How to connect to the MCP server
    transport: DynComponent,

    /// Sender for messages to the conductor
    conductor_tx: mpsc::Sender<ConductorMessage>,

    /// Receiver for messages from the conductor to the MCP client
    to_mcp_client_rx: mpsc::Receiver<MessageAndCx>,
}

impl McpBridgeConnectionActor {
    pub fn new(
        component: impl Component,
        conductor_tx: mpsc::Sender<ConductorMessage>,
        to_mcp_client_rx: mpsc::Receiver<MessageAndCx>,
    ) -> Self {
        Self {
            transport: DynComponent::new(component),
            conductor_tx,
            to_mcp_client_rx,
        }
    }

    pub async fn run(self, connection_id: String) -> Result<(), sacp::Error> {
        info!(connection_id, "MCP bridge connected");

        let McpBridgeConnectionActor {
            transport,
            mut conductor_tx,
            to_mcp_client_rx,
        } = self;

        let result = JrHandlerChain::new()
            .name(format!("mpc-client-to-conductor({connection_id})"))
            // When we receive a message from the MCP client, forward it to the conductor
            .on_receive_message({
                let mut conductor_tx = conductor_tx.clone();
                let connection_id = connection_id.clone();
                async move |message: sacp::MessageAndCx| {
                    conductor_tx
                        .send(ConductorMessage::McpClientToMcpServer {
                            connection_id: connection_id.clone(),
                            message,
                        })
                        .await
                        .map_err(|_| sacp::Error::internal_error())
                }
            })
            // When we receive messages from the conductor, forward them to the MCP client
            .connect_to(transport)?
            .with_client(async move |mcp_client_cx| {
                let mut to_mcp_client_rx = to_mcp_client_rx;
                while let Some(message) = to_mcp_client_rx.next().await {
                    mcp_client_cx.send_proxied_message(message)?;
                }
                Ok(())
            })
            .await;

        conductor_tx
            .send(ConductorMessage::McpConnectionDisconnected {
                notification: McpDisconnectNotification {
                    connection_id,
                    meta: None,
                },
            })
            .await
            .map_err(|_| sacp::Error::internal_error())?;

        result
    }
}
