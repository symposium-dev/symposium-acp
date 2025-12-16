//! MCP server support for providing MCP tools over ACP.
//!
//! This module provides the infrastructure for building MCP servers that
//! integrate with ACP connections.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use sacp::mcp_server::{McpServer, McpServiceRegistry, McpTool};
//!
//! // Create an MCP server with tools
//! let server = McpServer::new()
//!     .instructions("A helpful assistant")
//!     .tool(MyTool);
//!
//! // Register it with the service registry
//! let registry = McpServiceRegistry::new()
//!     .with_mcp_server("my-server", server)?;
//!
//! // Use the registry in your connection
//! UntypedRole::builder()
//!     .with_handler(registry)
//!     .serve(connection)
//!     .await?;
//! ```

mod registry;
mod server;
mod tool;

pub use registry::{McpContext, McpServiceRegistry};
pub use server::McpServer;
pub use tool::McpTool;
