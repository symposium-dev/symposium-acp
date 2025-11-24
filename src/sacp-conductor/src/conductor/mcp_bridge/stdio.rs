use std::{collections::HashMap, net::SocketAddr};

use futures::{SinkExt, StreamExt as _, channel::mpsc};
use sacp::{BoxFuture, JrConnectionCx, JrHandlerChain, MessageAndCx};
use sacp_proxy::McpDisconnectNotification;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};
use tracing::info;

use crate::conductor::ConductorMessage;

use super::{
    McpBridgeConnection, McpBridgeConnectionActor as McpBridgeConnectionActorTrait,
    McpBridgeListener, McpPort,
};

/// Actor that handles a stdio-based MCP bridge connection over TCP.
///
/// Bridges between MCP clients connecting via TCP (stdio transport)
/// and the conductor's ACP message flow.
#[derive(Debug)]
pub struct StdioMcpBridgeConnectionActor {
    /// TCP stream to the MCP client
    stream: TcpStream,

    /// Socket address we are connected on
    #[expect(dead_code)]
    addr: SocketAddr,

    /// Sender for messages to the conductor
    conductor_tx: mpsc::Sender<ConductorMessage>,

    /// Receiver for messages from the conductor to the MCP client
    to_mcp_client_rx: mpsc::Receiver<MessageAndCx>,
}

impl StdioMcpBridgeConnectionActor {
    pub fn new(
        stream: TcpStream,
        addr: SocketAddr,
        conductor_tx: mpsc::Sender<ConductorMessage>,
        to_mcp_client_rx: mpsc::Receiver<MessageAndCx>,
    ) -> Self {
        Self {
            stream,
            addr,
            conductor_tx,
            to_mcp_client_rx,
        }
    }
}

impl McpBridgeConnectionActorTrait for StdioMcpBridgeConnectionActor {
    fn run(self: Box<Self>, connection_id: String) -> BoxFuture<'static, Result<(), sacp::Error>> {
        Box::pin(async move {
            info!(connection_id, "Stdio bridge connected");

            let StdioMcpBridgeConnectionActor {
                stream,
                addr: _,
                mut conductor_tx,
                to_mcp_client_rx,
            } = *self;

            let (read_half, write_half) = stream.into_split();

            // Establish bidirectional JSON-RPC connection
            // The bridge will send MCP requests (tools/call, etc.) to the conductor
            // The conductor can also send responses back
            let transport = sacp::ByteStreams::new(write_half.compat_write(), read_half.compat());

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
        })
    }
}

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
                let (stream, addr) = listener
                    .accept()
                    .await
                    .map_err(sacp::Error::into_internal_error)?;

                let (to_mcp_client_tx, to_mcp_client_rx) = mpsc::channel(128);

                conductor_tx
                    .send(ConductorMessage::McpConnectionReceived {
                        acp_url: acp_url.clone(),
                        session_id: session_id.clone(),
                        actor: Box::new(StdioMcpBridgeConnectionActor::new(
                            stream,
                            addr,
                            conductor_tx.clone(),
                            to_mcp_client_rx,
                        )),
                        connection: McpBridgeConnection::new(to_mcp_client_tx),
                    })
                    .await
                    .map_err(|_| sacp::Error::internal_error())?;
            }
        }
    })?;

    Ok(tcp_port)
}
