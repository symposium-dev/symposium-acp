//! Elizacp agent component wrapper for testing

use sacp::Component;
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub struct ElizacpAgentComponent;

impl Component for ElizacpAgentComponent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Create duplex channels for bidirectional communication
        let (elizacp_write, client_read) = duplex(8192);
        let (client_write, elizacp_read) = duplex(8192);

        let elizacp_transport =
            sacp::ByteStreams::new(elizacp_write.compat_write(), elizacp_read.compat());

        let client_transport =
            sacp::ByteStreams::new(client_write.compat_write(), client_read.compat());

        // Spawn elizacp in a background task
        tokio::spawn(async move {
            if let Err(e) = elizacp::run_elizacp(elizacp_transport).await {
                tracing::error!("Elizacp error: {}", e);
            }
        });

        // Serve the client with the transport connected to elizacp
        client_transport.serve(client).await
    }
}

pub fn create() -> sacp::DynComponent {
    sacp::DynComponent::new(ElizacpAgentComponent)
}
