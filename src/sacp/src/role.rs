//! Role types for ACP connections.
//!
//! Roles represent the logical identity of an endpoint in an ACP connection.
//! Each role has a counterpart (who it connects to) and may have multiple peers
//! (who it can exchange messages with).

use std::{any::TypeId, fmt::Debug, future::Future, hash::Hash};

use crate::{Handled, ConnectionTo, MessageCx, RemoteStyle};

/// The role that an endpoint plays in an ACP connection.
///
/// Roles are the fundamental building blocks of ACP's type system:
/// - [`Client`] connects to [`Agent`]
/// - [`Agent`] connects to [`Client`]
/// - [`Proxy`] connects to [`Conductor`]
/// - [`Conductor`] connects to [`Proxy`]
///
/// Each role determines:
/// - Who the counterpart is (via [`Role::Counterpart`])
/// - How unhandled messages are processed (via [`Role::default_message_handler`])
pub trait Role: Debug + Copy + Send + Sync + 'static + Eq + Ord + Hash + Default {
    /// The role that this endpoint connects to.
    ///
    /// For example:
    /// - `Client::Counterpart = Agent`
    /// - `Agent::Counterpart = Client`
    /// - `Proxy::Counterpart = Conductor`
    /// - `Conductor::Counterpart = Proxy`
    type Counterpart: Role<Counterpart = Self>;

    /// Method invoked when there is no defined message handler.
    ///
    /// Returns `Handled::No` by default, which will cause an error response
    /// to be sent for requests.
    fn default_message_handler(
        message: MessageCx,
        #[expect(unused_variables)] cx: ConnectionTo<Self::Counterpart>,
    ) -> impl Future<Output = Result<Handled<MessageCx>, crate::Error>> + Send {
        async move {
            Ok(Handled::No {
                message,
                retry: false,
            })
        }
    }
}

/// Declares that a role can send messages to a specific peer.
///
/// Most roles only communicate with their counterpart, but some (like [`Proxy`])
/// can communicate with multiple peers:
/// - `Proxy: HasPeer<Client>` - proxy can send/receive from clients
/// - `Proxy: HasPeer<Agent>` - proxy can send/receive from agents
/// - `Proxy: HasPeer<Conductor>` - proxy can send/receive from its conductor
///
/// The [`RemoteStyle`] determines how messages are transformed:
/// - [`RemoteStyle::Counterpart`] - pass through unchanged
/// - [`RemoteStyle::Predecessor`] - pass through, but reject wrapped messages
/// - [`RemoteStyle::Successor`] - wrap in a [`SuccessorMessage`] envelope
///
/// [`SuccessorMessage`]: crate::schema::SuccessorMessage
pub trait HasPeer<Peer: Role>: Role {
    /// Returns the remote style for sending to this peer.
    fn remote_style(peer: Peer) -> RemoteStyle;
}

/// Unique identifier for a role instance.
///
/// Used to identify the source/destination of messages when multiple
/// peers are possible on a single connection.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum RoleId {
    /// Singleton role identified by type name and type ID.
    Singleton(&'static str, TypeId),
}

impl RoleId {
    /// Create the role ID for a singleton role type.
    fn from_singleton<R: Role>() -> RoleId {
        RoleId::Singleton(std::any::type_name::<R>(), TypeId::of::<R>())
    }
}

/// Extension trait for getting the role ID from a role instance.
pub trait RoleExt: Role {
    /// Return a RoleId that uniquely identifies this role.
    fn role_id(&self) -> RoleId {
        RoleId::from_singleton::<Self>()
    }
}

impl<R: Role> RoleExt for R {}

// ============================================================================
// Role implementations
// ============================================================================

/// The client role - typically an IDE or CLI that controls an agent.
///
/// Clients send prompts and receive responses from agents.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Client;

impl Role for Client {
    type Counterpart = Agent;
}

impl HasPeer<Agent> for Client {
    fn remote_style(_peer: Agent) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

/// The agent role - typically an LLM that responds to prompts.
///
/// Agents receive prompts from clients and respond with answers,
/// potentially invoking tools along the way.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Agent;

impl Role for Agent {
    type Counterpart = Client;
}

impl HasPeer<Client> for Agent {
    fn remote_style(_peer: Client) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

/// The proxy role - an intermediary that can intercept and modify messages.
///
/// Proxies sit between a client and an agent (or another proxy), and can:
/// - Add tools via MCP servers
/// - Filter or transform messages
/// - Inject additional context
///
/// Proxies connect to a [`Conductor`] which orchestrates the proxy chain.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Proxy;

impl Role for Proxy {
    type Counterpart = Conductor;
}

impl HasPeer<Client> for Proxy {
    fn remote_style(_peer: Client) -> RemoteStyle {
        RemoteStyle::Predecessor
    }
}

impl HasPeer<Agent> for Proxy {
    fn remote_style(_peer: Agent) -> RemoteStyle {
        RemoteStyle::Successor
    }
}

/// The conductor role - orchestrates proxy chains.
///
/// Conductors manage connections between clients, proxies, and agents,
/// routing messages through the appropriate proxy chain.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Conductor;

impl Role for Conductor {
    type Counterpart = Proxy;
}

impl HasPeer<Proxy> for Conductor {
    fn remote_style(_peer: Proxy) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}
