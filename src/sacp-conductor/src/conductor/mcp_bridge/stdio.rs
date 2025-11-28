use futures::{SinkExt, channel::mpsc};
use sacp::MessageAndCx;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

use crate::conductor::ConductorMessage;

use super::{McpBridgeConnection, McpBridgeConnectionActor};

/// Runs the stdio bridge TCP listener, accepting connections and creating bridge actors for each.
///
/// Loops indefinitely, accepting incoming TCP connections and spawning an MCP bridge actor
/// for each connection to handle bidirectional message forwarding between the MCP client
/// and the conductor.
pub async fn run_tcp_listener(
    tcp_listener: TcpListener,
    acp_url: String,
    mut conductor_tx: mpsc::Sender<ConductorMessage>,
) -> Result<(), sacp::Error> {
    // Accept connections
    loop {
        let (stream, _addr) = tcp_listener
            .accept()
            .await
            .map_err(sacp::Error::into_internal_error)?;

        let (to_mcp_client_tx, to_mcp_client_rx) = mpsc::channel(128);

        conductor_tx
            .send(ConductorMessage::McpConnectionReceived {
                acp_url: acp_url.clone(),
                actor: make_stdio_actor(stream, conductor_tx.clone(), to_mcp_client_rx),
                connection: McpBridgeConnection::new(to_mcp_client_tx),
            })
            .await
            .map_err(|_| sacp::Error::internal_error())?;
    }
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
