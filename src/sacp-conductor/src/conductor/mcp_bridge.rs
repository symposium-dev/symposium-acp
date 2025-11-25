pub mod actor;
pub mod http;
pub mod stdio;

use std::collections::HashMap;
use std::path::PathBuf;

use futures::{SinkExt, channel::mpsc};
use sacp;
use sacp::schema::McpServer;
use sacp::{JrConnectionCx, MessageAndCx};
use tokio::net::TcpListener;
use tracing::info;

pub use self::actor::McpBridgeConnectionActor;
use crate::conductor::ConductorMessage;

/// Maintains bridges for MCP message routing.
#[derive(Default)]
pub struct McpBridgeListeners {
    /// Mapping of acp:$UUID URLs to TCP bridge information for MCP message routing
    listeners: HashMap<String, McpBridgeListener>,
}

/// Information about an MCP bridge that is listening for connections from MCP clients.
#[derive(Clone, Debug)]
pub(super) struct McpBridgeListener {
    /// The replacement MCP server
    pub server: McpServer,
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
    /// 3. Transforms the server to use either stdio or HTTP transport depending on bridge mode
    /// 4. Returns a oneshot sender for delivering the session_id to the listener
    ///
    /// Returns `Some(session_id_tx)` if a listener was spawned, `None` otherwise.
    pub async fn transform_mcp_server(
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
        let transformed = self
            .spawn_bridge(cx, name, url, conductor_tx, mcp_bridge_mode, session_id_rx)
            .await?;
        *mcp_server = transformed;
        Ok(Some(session_id_tx))
    }

    /// Spawn a bridge listener (HTTP or stdio) for an MCP server with ACP transport
    async fn spawn_bridge(
        &mut self,
        cx: &JrConnectionCx,
        server_name: &str,
        acp_url: &str,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        mcp_bridge_mode: &crate::McpBridgeMode,
        session_id_rx: futures::channel::oneshot::Receiver<sacp::schema::SessionId>,
    ) -> anyhow::Result<McpServer> {
        // If there is already a listener for the ACP URL, return its server
        if let Some(listener) = self.listeners.get(acp_url) {
            return Ok(listener.server.clone());
        }

        // Bind to ephemeral port
        let tcp_listener = TcpListener::bind("127.0.0.1:0").await?;
        let tcp_port = tcp_listener.local_addr()?.port();

        info!(acp_url = acp_url, tcp_port, "Bound listener for MCP bridge");

        let new_server = match mcp_bridge_mode {
            crate::McpBridgeMode::Stdio { conductor_command } => McpServer::Stdio {
                name: server_name.to_string(),
                command: PathBuf::from(&conductor_command[0]),
                args: conductor_command[1..]
                    .iter()
                    .cloned()
                    .chain(vec!["mcp".to_string(), format!("{tcp_port}")])
                    .collect(),
                env: Default::default(),
            },

            crate::McpBridgeMode::Http => McpServer::Http {
                name: server_name.to_string(),
                url: format!("http://localhost:{tcp_port}"),
                headers: vec![],
            },
        };

        // remember for later
        self.listeners.insert(
            acp_url.to_string(),
            McpBridgeListener {
                server: new_server.clone(),
            },
        );

        cx.spawn({
            let acp_url = acp_url.to_string();
            let conductor_tx = conductor_tx.clone();
            let mcp_bridge_mode = mcp_bridge_mode.clone();
            async move {
                info!(
                    acp_url = acp_url,
                    tcp_port, "Waiting for session_id before accepting connections"
                );

                // Block waiting for session_id before accepting any connections
                let session_id = session_id_rx.await.map_err(|_| {
                    sacp::Error::internal_error()
                        .with_data("session_id channel closed before receiving session_id")
                })?;

                info!(
                    acp_url = acp_url,
                    tcp_port,
                    ?session_id,
                    "Session ID received, now accepting bridge connections"
                );

                match mcp_bridge_mode {
                    crate::McpBridgeMode::Stdio {
                        conductor_command: _,
                    } => {
                        stdio::run_tcp_listener(tcp_listener, acp_url, session_id, conductor_tx)
                            .await
                    }
                    crate::McpBridgeMode::Http => {
                        http::run_http_listener(tcp_listener, acp_url, session_id, conductor_tx)
                            .await
                    }
                }
            }
        })?;

        Ok(new_server)
    }
}
