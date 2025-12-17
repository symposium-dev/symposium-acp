use std::sync::Arc;

use crate::{DynComponent, JrRole, mcp_server::McpContext};

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
/// use sacp::{DynComponent, JrRole};
///
/// struct MyMcpServer {
///     name: String,
/// }
///
/// impl<Role: JrRole> McpServerConnect<Role> for MyMcpServer {
///     fn name(&self) -> String {
///         self.name.clone()
///     }
///
///     fn connect(&self, cx: McpContext<Role>) -> DynComponent {
///         // Create and return a component that handles MCP requests
///         DynComponent::new(MyMcpComponent::new(cx))
///     }
/// }
/// ```
pub trait McpServerConnect<Role: JrRole>: Send + Sync + 'static {
    /// The name of the MCP server, used to identify it in session responses.
    fn name(&self) -> String;

    /// Create a component to service a new MCP connection.
    ///
    /// This is called each time an agent connects to this MCP server. The returned
    /// component will handle MCP protocol messages for that connection.
    ///
    /// The [`McpContext`] provides access to the ACP connection context and the
    /// server's ACP URL.
    fn connect(&self, cx: McpContext<Role>) -> DynComponent;
}

impl<Role: JrRole, S: ?Sized + McpServerConnect<Role>> McpServerConnect<Role> for Box<S> {
    fn name(&self) -> String {
        S::name(self)
    }

    fn connect(&self, cx: McpContext<Role>) -> DynComponent {
        S::connect(self, cx)
    }
}

impl<Role: JrRole, S: ?Sized + McpServerConnect<Role>> McpServerConnect<Role> for Arc<S> {
    fn name(&self) -> String {
        S::name(self)
    }

    fn connect(&self, cx: McpContext<Role>) -> DynComponent {
        S::connect(self, cx)
    }
}
