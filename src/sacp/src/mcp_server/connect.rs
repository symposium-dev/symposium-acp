use std::sync::Arc;

use crate::{DynComponent, JrLink, mcp::McpServerToClient, mcp_server::McpContext};

/// Trait for types that can create MCP server connections.
///
/// Implement this trait to create custom MCP servers. Each call to [`connect`](Self::connect)
/// should return a new [`Component`](crate::Component) that serves MCP requests for a single
/// connection.
///
/// # Example
///
/// ```rust,ignore
/// use sacp::mcp_server::{McpServerConnect, McpContext};
/// use sacp::{DynComponent, JrLink};
///
/// struct MyMcpServer {
///     name: String,
/// }
///
/// impl<Link: JrLink> McpServerConnect<Link> for MyMcpServer {
///     fn name(&self) -> String {
///         self.name.clone()
///     }
///
///     fn connect(&self, cx: McpContext<Link>) -> DynComponent<McpServerToClient> {
///         // Create and return a component that handles MCP requests
///         DynComponent::new(MyMcpComponent::new(cx))
///     }
/// }
/// ```
pub trait McpServerConnect<Link: JrLink>: Send + Sync + 'static {
    /// The name of the MCP server, used to identify it in session responses.
    fn name(&self) -> String;

    /// Create a component to service a new MCP connection.
    ///
    /// This is called each time an agent connects to this MCP server. The returned
    /// component will handle MCP protocol messages for that connection.
    ///
    /// The [`McpContext`] provides access to the ACP connection context and the
    /// server's ACP URL.
    fn connect(&self, cx: McpContext<Link>) -> DynComponent<McpServerToClient>;
}

impl<Link: JrLink, S: ?Sized + McpServerConnect<Link>> McpServerConnect<Link> for Box<S> {
    fn name(&self) -> String {
        S::name(self)
    }

    fn connect(&self, cx: McpContext<Link>) -> DynComponent<McpServerToClient> {
        S::connect(self, cx)
    }
}

impl<Link: JrLink, S: ?Sized + McpServerConnect<Link>> McpServerConnect<Link> for Arc<S> {
    fn name(&self) -> String {
        S::name(self)
    }

    fn connect(&self, cx: McpContext<Link>) -> DynComponent<McpServerToClient> {
        S::connect(self, cx)
    }
}
