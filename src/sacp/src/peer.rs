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

/// Peer representing the **client** (e.g., the editor). The client is the controlling the agent by sending prompts and receiving answers.
/// Clients also select tools to make available for use by agents.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientPeer;

impl JrPeer for ClientPeer {}

/// Peer representing the **agent** (e.g., the LLM). The agent responds to prompts send by the client with answers and tool invocations.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentPeer;

impl JrPeer for AgentPeer {}

/// Peer representing the **conductor**. The conductor is a special client that is used only when authoring a proxy; the conductor
/// arranges proxies and coordinates their messages.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorPeer;

impl JrPeer for ConductorPeer {}

/// Peer representing a **proxy**. A proxy is a special agent that can not only receive prompts and so forth but also communicates with its successor,
/// which may be another proxy or an agent.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProxyPeer;

impl JrPeer for ProxyPeer {}
