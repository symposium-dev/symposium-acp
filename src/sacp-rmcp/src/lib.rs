//! # sacp-rmcp - rmcp integration for SACP
//!
//! This crate provides integration between [rmcp](https://docs.rs/rmcp) MCP servers
//! and the SACP MCP server framework.
//!
//! ## Usage
//!
//! Create an MCP server from an rmcp service:
//!
//! ```ignore
//! use sacp::mcp_server::McpServer;
//! use sacp_rmcp::RmcpServer;
//!
//! let server = McpServer::new(RmcpServer::new("my-server", || MyRmcpService::new()));
//!
//! // Use as a handler
//! ProxyToConductor::builder()
//!     .with_handler(server)
//!     .serve(client)
//!     .await?;
//! ```

use rmcp::ServiceExt;
use sacp::mcp_server::{McpContext, McpServerConnect};
use sacp::{ByteStreams, Component, DynComponent, JrRole};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Wrapper that implements [`McpServerConnect`] for rmcp services.
///
/// Use this to integrate existing rmcp-based MCP servers with the SACP framework.
///
/// # Example
///
/// ```ignore
/// use sacp::mcp_server::McpServer;
/// use sacp_rmcp::RmcpServer;
///
/// // Create an MCP server from an rmcp service factory
/// let server = McpServer::new(RmcpServer::new("my-server", || MyRmcpService::new()));
/// ```
pub struct RmcpServer<F> {
    name: String,
    make_service: F,
}

impl<F> RmcpServer<F> {
    /// Create a new rmcp server wrapper.
    ///
    /// # Parameters
    ///
    /// - `name`: The name of the server (used in session responses).
    /// - `make_service`: A function that creates a new rmcp service instance.
    ///   This is called for each new connection.
    pub fn new(name: impl ToString, make_service: F) -> Self {
        Self {
            name: name.to_string(),
            make_service,
        }
    }
}

impl<Role, F, S> McpServerConnect<Role> for RmcpServer<F>
where
    Role: JrRole,
    F: Fn() -> S + Send + Sync + 'static,
    S: rmcp::Service<rmcp::RoleServer>,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    fn connect(&self, _cx: McpContext<Role>) -> DynComponent {
        let service = (self.make_service)();
        DynComponent::new(RmcpServerComponent { service })
    }
}

/// Component wrapper for rmcp services.
struct RmcpServerComponent<S> {
    service: S,
}

impl<S> Component for RmcpServerComponent<S>
where
    S: rmcp::Service<rmcp::RoleServer>,
{
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Create tokio byte streams that rmcp expects
        let (mcp_server_stream, mcp_client_stream) = tokio::io::duplex(8192);
        let (mcp_server_read, mcp_server_write) = tokio::io::split(mcp_server_stream);
        let (mcp_client_read, mcp_client_write) = tokio::io::split(mcp_client_stream);

        // Create ByteStreams component for the client side
        let byte_streams =
            ByteStreams::new(mcp_client_write.compat_write(), mcp_client_read.compat());

        // Spawn task to connect byte_streams to the provided client
        tokio::spawn(async move {
            let _ = byte_streams.serve(client).await;
        });

        // Run the rmcp server with the server side of the duplex stream
        let running_server = self
            .service
            .serve((mcp_server_read, mcp_server_write))
            .await
            .map_err(sacp::Error::into_internal_error)?;

        // Wait for the server to finish
        running_server
            .waiting()
            .await
            .map(|_quit_reason| ())
            .map_err(sacp::Error::into_internal_error)
    }
}
