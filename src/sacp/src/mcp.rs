use crate::{
    HasDefaultPeer, HasPeer, JrLink, JrPeer, jsonrpc::JrConnectionBuilder,
    jsonrpc::handlers::NullHandler, peer::RemoteStyle,
};

/// The MCP client endpoint.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpClient;

impl JrPeer for McpClient {}

/// The MCP server endpoint.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpServerEnd;

impl JrPeer for McpServerEnd {}

/// An MCP client's connection to an MCP server.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpClientToServer;

impl JrLink for McpClientToServer {
    type ConnectsTo = McpServerToClient;
    type State = ();
}

impl HasDefaultPeer for McpClientToServer {
    type DefaultPeer = McpServerEnd;
}

impl HasPeer<McpServerEnd> for McpClientToServer {
    fn remote_style(_: McpServerEnd) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

impl McpClientToServer {
    /// Create a connection builder for an MCP client talking to an MCP server.
    pub fn builder() -> JrConnectionBuilder<NullHandler<McpClientToServer>> {
        JrConnectionBuilder::new(McpClientToServer)
    }
}

/// An MCP server's connection to an MCP client.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpServerToClient;

impl JrLink for McpServerToClient {
    type ConnectsTo = McpClientToServer;
    type State = ();
}

impl HasDefaultPeer for McpServerToClient {
    type DefaultPeer = McpClient;
}

impl HasPeer<McpClient> for McpServerToClient {
    fn remote_style(_: McpClient) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

impl McpServerToClient {
    /// Create a connection builder for an MCP server talking to an MCP client.
    pub fn builder() -> JrConnectionBuilder<NullHandler<McpServerToClient>> {
        JrConnectionBuilder::new(McpServerToClient)
    }
}
