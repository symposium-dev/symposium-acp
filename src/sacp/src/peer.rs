//! Peer types for ACP and related protocols.
//!
//! Peers are logical destinations for messages (e.g., Client, Agent, McpServer).

use std::{any::TypeId, fmt::Debug, hash::Hash};

/// A logical destination for messages (e.g., Client, Agent, McpServer).
pub trait JrPeer: Debug + Clone + Send + Sync + 'static + Eq + Ord + Hash + Default {
    /// Return a PeerId that identifies self such that if `x.peer_id() == y.peer_id()` then `x == y`.
    fn peer_id(&self) -> PeerId;
}

/// Unique identify for a peer.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum PeerId {
    /// Singleton peer identified by type-id.
    TypeId(&'static str, TypeId),
}

impl PeerId {
    /// Create the peer-id for a singleton type.
    ///
    /// Crate private out of an excess of caution, ensures that Peer cannot be implemented
    /// outside of this crate in a useful way.
    pub(crate) fn from_singleton<Peer: JrPeer>(peer: &Peer) -> PeerId {
        assert_eq!(size_of_val(peer), 0);
        PeerId::TypeId(std::any::type_name::<Peer>(), TypeId::of::<Peer>())
    }
}

/// A generic peer for untyped connections.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UntypedPeer;

impl JrPeer for UntypedPeer {
    fn peer_id(&self) -> PeerId {
        PeerId::from_singleton(self)
    }
}

/// Peer representing the **client** (e.g., the editor). The client is the controlling the agent by sending prompts and receiving answers.
/// Clients also select tools to make available for use by agents.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientPeer;

impl JrPeer for ClientPeer {
    fn peer_id(&self) -> PeerId {
        PeerId::from_singleton(self)
    }
}

/// Peer representing the **agent** (e.g., the LLM). The agent responds to prompts send by the client with answers and tool invocations.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentPeer;

impl JrPeer for AgentPeer {
    fn peer_id(&self) -> PeerId {
        PeerId::from_singleton(self)
    }
}

/// Peer representing the **conductor**. The conductor is a special client that is used only when authoring a proxy; the conductor
/// arranges proxies and coordinates their messages.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorPeer;

impl JrPeer for ConductorPeer {
    fn peer_id(&self) -> PeerId {
        PeerId::from_singleton(self)
    }
}

/// Peer representing a **proxy**. A proxy is a special agent that can not only receive prompts and so forth but also communicates with its successor,
/// which may be another proxy or an agent.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProxyPeer;

impl JrPeer for ProxyPeer {
    fn peer_id(&self) -> PeerId {
        PeerId::from_singleton(self)
    }
}
