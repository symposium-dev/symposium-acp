use std::collections::HashMap;

use futures::{SinkExt, channel::mpsc};
use sacp::{JrConnectionCx, MessageAndCx};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};
use tracing::info;

use crate::conductor::ConductorMessage;

use super::{McpBridgeConnection, McpBridgeConnectionActor, McpBridgeListener, McpPort};

/// Spawns a TCP listener for an MCP bridge and stores the mapping.
///
/// Binds to `localhost:0` to get an ephemeral port, then stores the
/// `acp_url → tcp_port` mapping in the listeners map.
///
/// The listener blocks on receiving the session_id before processing connections.
///
/// Returns the bound port number.
pub async fn spawn_tcp_listener(
    listeners: &mut HashMap<String, McpBridgeListener>,
    cx: &JrConnectionCx,
    acp_url: &str,
    conductor_tx: mpsc::Sender<ConductorMessage>,
    session_id_rx: futures::channel::oneshot::Receiver<sacp::schema::SessionId>,
) -> anyhow::Result<McpPort> {
    // If there is already a listener for the ACP URL, return its TCP port
    if let Some(listener) = listeners.get(acp_url) {
        return Ok(listener.tcp_port);
    }

    // Bind to ephemeral port
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let tcp_port = McpPort {
        tcp_port: listener.local_addr()?.port(),
    };

    info!(
        acp_url = acp_url,
        tcp_port.tcp_port, "Bound TCP listener for MCP bridge"
    );

    // Store mapping for message routing
    listeners.insert(acp_url.to_string(), McpBridgeListener { tcp_port });

    // Accept connections from `conductor mcp $PORT`
    cx.spawn({
        let acp_url = acp_url.to_string();
        let mut conductor_tx = conductor_tx.clone();
        async move {
            info!(
                acp_url = acp_url,
                tcp_port.tcp_port, "Waiting for session_id before accepting connections"
            );

            // Block waiting for session_id before accepting any connections
            let session_id = session_id_rx.await.map_err(|_| {
                sacp::Error::internal_error()
                    .with_data("session_id channel closed before receiving session_id")
            })?;

            info!(
                acp_url = acp_url,
                tcp_port.tcp_port,
                ?session_id,
                "Session ID received, now accepting bridge connections"
            );

            // Accept connections
            loop {
                let (stream, _addr) = listener
                    .accept()
                    .await
                    .map_err(sacp::Error::into_internal_error)?;

                let (to_mcp_client_tx, to_mcp_client_rx) = mpsc::channel(128);

                conductor_tx
                    .send(ConductorMessage::McpConnectionReceived {
                        acp_url: acp_url.clone(),
                        session_id: session_id.clone(),
                        actor: make_stdio_actor(stream, conductor_tx.clone(), to_mcp_client_rx),
                        connection: McpBridgeConnection::new(to_mcp_client_tx),
                    })
                    .await
                    .map_err(|_| sacp::Error::internal_error())?;
            }
        }
    })?;

    Ok(tcp_port)
}

fn make_stdio_actor(
    stream: TcpStream,
    conductor_tx: mpsc::Sender<ConductorMessage>,
    to_mcp_client_rx: mpsc::Receiver<MessageAndCx>,
) -> McpBridgeConnectionActor {
    let (read_half, write_half) = stream.into_split();

    // Establish bidirectional JSON-RPC connection
    // The bridge will send MCP requests (tools/call, etc.) to the conductor
    // The conductor can also send responses back
    let transport = sacp::ByteStreams::new(write_half.compat_write(), read_half.compat());

    McpBridgeConnectionActor::new(transport, conductor_tx, to_mcp_client_rx)
}
