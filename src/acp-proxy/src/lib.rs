/// Proxying MCP messages over ACP.
mod mcp_over_acp;
pub use mcp_over_acp::*;

/// Proxying and sending messages to/from the successor component
mod to_from_successor;
pub use to_from_successor::*;

mod mcp_server;
pub use mcp_server::*;
