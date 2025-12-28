//! Peer types for ACP and related protocols.
//!
//! Peers are logical destinations for messages (e.g., Client, Agent, McpServer).

use std::{fmt::Debug, hash::Hash};

/// A logical destination for messages (e.g., Client, Agent, McpServer).
pub trait JrPeer: Debug + Copy + Send + Sync + 'static + Eq + Ord + Hash + Default {}

/// A generic peer for untyped connections.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UntypedPeer;

impl JrPeer for UntypedPeer {}

/// Peer representing the client direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientPeer;

impl JrPeer for ClientPeer {}

/// Peer representing the agent direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentPeer;

impl JrPeer for AgentPeer {}

/// Peer representing the conductor direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorPeer;

impl JrPeer for ConductorPeer {}
