//! # sacp-rmcp - rmcp integration for SACP
//!
//! This crate provides integration between [rmcp](https://docs.rs/rmcp) MCP servers
//! and the SACP MCP server framework.
//!
//! ## Usage
//!
//! Create an MCP server from an rmcp service using the extension trait:
//!
//! ```ignore
//! use sacp::mcp_server::McpServer;
//! use sacp_rmcp::McpServerExt;
//!
//! let server = McpServer::from_rmcp("my-server", MyRmcpService::new);
//!
//! // Use as a handler
//! ProxyToConductor::builder()
//!     .with_handler(server)
//!     .serve(client)
//!     .await?;
//! ```

use rmcp::ServiceExt;
use sacp::mcp_server::{McpContext, McpServer, McpServerConnect};
use sacp::{Agent, ByteStreams, Component, DynComponent, HasEndpoint, JrRole};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub trait McpServerExt<Role: JrRole>
where
    Role: HasEndpoint<Agent>,
{
    /// Create an MCP server from something that implements the [`McpServerConnect`] trait.
    ///
    /// # See also
    ///
    /// See [`Self::builder`] to construct MCP servers from Rust code.
    fn from_rmcp<S>(
        name: impl ToString,
        new_fn: impl Fn() -> S + Send + Sync + 'static,
    ) -> McpServer<Role>
    where
        S: rmcp::Service<rmcp::RoleServer>,
    {
        struct RmcpServer<F> {
            name: String,
            new_fn: F,
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
                let service = (self.new_fn)();
                DynComponent::new(RmcpServerComponent { service })
            }
        }

        McpServer::new(RmcpServer {
            name: name.to_string(),
            new_fn,
        })
    }
}

impl<Role: JrRole> McpServerExt<Role> for McpServer<Role> where Role: HasEndpoint<Agent> {}

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
