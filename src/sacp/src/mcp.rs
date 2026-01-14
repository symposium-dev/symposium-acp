use crate::{
    HasDefaultPeer, HasPeer, JrLink, JrPeer,
    jsonrpc::{JrConnectionBuilder, handlers::NullHandler},
    link::RemoteStyle,
    peer::PeerId,
};

/// The MCP client endpoint.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpClientPeer;

impl JrPeer for McpClientPeer {
    fn peer_id(&self) -> PeerId {
        PeerId::from_singleton(self)
    }
}

/// The MCP server endpoint.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpServerPeer;

impl JrPeer for McpServerPeer {
    fn peer_id(&self) -> PeerId {
        PeerId::from_singleton(self)
    }
}

/// An MCP client's connection to an MCP server.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpClientToServer;

impl JrLink for McpClientToServer {
    type ConnectsTo = McpServerToClient;
    type State = ();
}

impl HasDefaultPeer for McpClientToServer {
    type DefaultPeer = McpServerPeer;
}

impl HasPeer<McpServerPeer> for McpClientToServer {
    fn remote_style(_: McpServerPeer) -> RemoteStyle {
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
    type DefaultPeer = McpClientPeer;
}

impl HasPeer<McpClientPeer> for McpServerToClient {
    fn remote_style(_: McpClientPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

impl McpServerToClient {
    /// Create a connection builder for an MCP server talking to an MCP client.
    pub fn builder() -> JrConnectionBuilder<NullHandler<McpServerToClient>> {
        JrConnectionBuilder::new(McpServerToClient)
    }
}
