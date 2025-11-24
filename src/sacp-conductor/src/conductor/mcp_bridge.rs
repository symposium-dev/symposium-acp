pub mod actor;
pub mod http;
pub mod stdio;

use std::collections::HashMap;

use futures::{SinkExt, channel::mpsc};
use sacp;
use sacp::schema::McpServer;
use sacp::{JrConnectionCx, MessageAndCx};
use tracing::info;

pub use self::actor::McpBridgeConnectionActor;
use crate::conductor::ConductorMessage;

/// Maintains bridges for MCP message routing.
#[derive(Default)]
pub struct McpBridgeListeners {
    /// Mapping of acp:$UUID URLs to TCP bridge information for MCP message routing
    listeners: HashMap<String, McpBridgeListener>,
}

/// TCP port on which an MCP bridge is listening.
#[derive(Copy, Clone, Debug)]
pub struct McpPort {
    pub tcp_port: u16,
}

/// Information about an MCP bridge that is listening for connections from MCP clients.
#[derive(Clone, Debug)]
pub(super) struct McpBridgeListener {
    /// The TCP port we bound for this bridge
    pub tcp_port: McpPort,
}

/// Connection handle for sending messages to an MCP client.
#[derive(Clone, Debug)]
pub struct McpBridgeConnection {
    /// Channel to send messages from MCP server (ACP proxy) to the MCP client (ACP agent).
    to_mcp_client_tx: mpsc::Sender<MessageAndCx>,
}

impl McpBridgeConnection {
    pub fn new(to_mcp_client_tx: mpsc::Sender<MessageAndCx>) -> Self {
        Self { to_mcp_client_tx }
    }

    pub async fn send(&mut self, message: MessageAndCx) -> Result<(), sacp::Error> {
        self.to_mcp_client_tx
            .send(message)
            .await
            .map_err(|_| sacp::Error::internal_error())
    }
}

impl McpBridgeListeners {
    /// Transforms MCP servers with `acp:$UUID` URLs for agents that need bridging.
    ///
    /// For each MCP server with an `acp:` URL:
    /// 1. Spawns a TCP listener on an ephemeral port
    /// 2. Stores the mapping for message routing
    /// 3. Transforms the server to use stdio transport pointing to `conductor mcp $PORT`
    /// 4. Returns a oneshot sender for delivering the session_id to the listener
    ///
    /// Returns `Some(session_id_tx)` if a listener was spawned, `None` otherwise.
    pub async fn transform_mcp_servers(
        &mut self,
        cx: &JrConnectionCx,
        mcp_server: &mut McpServer,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        mcp_bridge_mode: &crate::McpBridgeMode,
    ) -> Result<Option<futures::channel::oneshot::Sender<sacp::schema::SessionId>>, sacp::Error>
    {
        use sacp::schema::McpServer;

        let McpServer::Http { name, url, headers } = mcp_server else {
            return Ok(None);
        };

        if !url.starts_with("acp:") {
            return Ok(None);
        }

        if !headers.is_empty() {
            return Err(sacp::Error::internal_error());
        }

        info!(
            server_name = name,
            acp_url = url,
            "Detected MCP server with ACP transport, spawning TCP bridge"
        );

        // Create oneshot channel for session_id delivery
        let (session_id_tx, session_id_rx) = futures::channel::oneshot::channel();

        // Transform to stdio transport pointing to conductor mcp process
        let transformed = match mcp_bridge_mode {
            crate::McpBridgeMode::Stdio { conductor_command } => {
                // Spawn TCP listener on ephemeral port
                let tcp_port = stdio::spawn_tcp_listener(
                    &mut self.listeners,
                    cx,
                    url,
                    conductor_tx.clone(),
                    session_id_rx,
                )
                .await?;

                info!(
                    server_name = name,
                    acp_url = url,
                    tcp_port.tcp_port,
                    "Spawned TCP listener for MCP bridge"
                );

                tracing::debug!(
                    conductor_command = ?conductor_command,
                    "Transforming MCP server to stdio"
                );
                let command = std::path::PathBuf::from(&conductor_command[0]);
                let mut args: Vec<String> = conductor_command[1..].to_vec();
                args.push("mcp".to_string());
                args.push(tcp_port.tcp_port.to_string());

                McpServer::Stdio {
                    name: name.clone(),
                    command,
                    args,
                    env: vec![],
                }
            }

            crate::McpBridgeMode::Http => {
                let tcp_port = http::spawn_http_listener(
                    &mut self.listeners,
                    cx,
                    url,
                    conductor_tx.clone(),
                    session_id_rx,
                )
                .await?;

                info!(
                    server_name = name,
                    acp_url = url,
                    port = tcp_port.tcp_port,
                    "Spawned HTTP listener for MCP bridge"
                );

                McpServer::Http {
                    name: name.clone(),
                    url: format!("http://127.0.0.1:{}", tcp_port.tcp_port),
                    headers: vec![],
                }
            }
        };

        *mcp_server = transformed;
        Ok(Some(session_id_tx))
    }
}
