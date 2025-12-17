use crate::{
    HasDefaultEndpoint, HasEndpoint, JrEndpoint, JrRole, jsonrpc::JrConnectionBuilder,
    jsonrpc::handlers::NullHandler, role::RemoteRoleStyle,
};

/// The MCP client endpoint.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpClient;

impl JrEndpoint for McpClient {}

/// The MCP server endpoint.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpServerEnd;

impl JrEndpoint for McpServerEnd {}

/// An MCP client's connection to an MCP server.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpClientToServer;

impl JrRole for McpClientToServer {
    type HandlerEndpoint = McpServerEnd;
    type State = ();
}

impl HasDefaultEndpoint for McpClientToServer {}

impl HasEndpoint<McpServerEnd> for McpClientToServer {
    fn remote_style(_: McpServerEnd) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl McpClientToServer {
    /// Create a connection builder for an MCP client talking to an MCP server.
    pub fn builder() -> JrConnectionBuilder<'static, NullHandler<McpClientToServer>> {
        JrConnectionBuilder::new(McpClientToServer)
    }
}

/// An MCP server's connection to an MCP client.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpServerToClient;

impl JrRole for McpServerToClient {
    type HandlerEndpoint = McpClient;
    type State = ();
}

impl HasDefaultEndpoint for McpServerToClient {}

impl HasEndpoint<McpClient> for McpServerToClient {
    fn remote_style(_: McpClient) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl McpServerToClient {
    /// Create a connection builder for an MCP server talking to an MCP client.
    pub fn builder() -> JrConnectionBuilder<'static, NullHandler<McpServerToClient>> {
        JrConnectionBuilder::new(McpServerToClient)
    }
}
