//! Role traits and Role types for ACP and related protocols.
//!
//! Roles are directional - they capture both "who I am" and "who I'm talking to".
//! For example, `ClientToAgent` is a client's connection to an agent.
//!
//! Endpoints are logical destinations for messages. Most roles have a single
//! implicit endpoint, but proxies can send to multiple endpoints (`Client` or `Agent`).

use std::{fmt::Debug, hash::Hash};

use crate::{
    Handled, JrConnectionCx, JrMessage, MessageCx, UntypedMessage,
    jsonrpc::JrConnectionBuilder,
    jsonrpc::handlers::NullHandler,
    schema::{
        InitializeProxyRequest, InitializeRequest, METHOD_INITIALIZE_PROXY,
        METHOD_SUCCESSOR_MESSAGE, SuccessorMessage,
    },
    util::MatchMessage,
    util::json_cast,
};

/// Trait for JSON-RPC connection roles.
///
/// The role determines what operations are valid on a connection and
/// provides role-specific behavior like handling unhandled messages.
#[expect(async_fn_in_trait)]
pub trait JrRole: Debug + Copy + Send + Sync + 'static + Eq + Ord + Hash + Default {
    /// The default endpoint type for handlers registered on this role.
    ///
    /// This determines which endpoint messages are assumed to come from when
    /// using `on_receive_request`, `on_receive_notification`, etc. without
    /// an explicit endpoint specification.
    ///
    /// For roles with a single counterpart (like `ClientToAgent`), this is
    /// typically that counterpart's endpoint. For roles that can receive from
    /// multiple endpoints (like proxies), this should be set to an explicit
    /// endpoint to avoid ambiguity.
    type HandlerEndpoint: JrEndpoint;

    /// Method invoked when there is no defined message handler.
    async fn default_message_handler(
        message: MessageCx,
        cx: JrConnectionCx<Self>,
    ) -> Result<(), crate::Error> {
        let method = message.method().to_string();
        message.respond_with_error(crate::Error::method_not_found().with_data(method), cx)
    }
}

/// A role that has a default endpoint for sending messages (the default is `JrRole::HandlerEndpoint`)
pub trait HasDefaultEndpoint: JrRole {}

/// A logical destination for messages (e.g., Client, Agent, McpServer).
pub trait JrEndpoint: Debug + Copy + Send + Sync + 'static + Eq + Ord + Hash + Default {}

/// Declares that a role can send messages to a specific endpoint.
pub trait HasEndpoint<End: JrEndpoint>: JrRole {
    /// Returns the remote role style for sending to this endpoint.
    fn remote_style(end: End) -> RemoteRoleStyle;
}

/// Describes how messages are transformed when sent to a remote endpoint.
#[derive(Debug)]
#[non_exhaustive]
pub enum RemoteRoleStyle {
    /// Pass each message through exactly as it is.
    Counterpart,

    /// Wrap messages in a [`SuccessorMessage`] envelope.
    Successor,
}

impl RemoteRoleStyle {
    pub(crate) fn transform_outgoing_message<M: JrMessage>(
        &self,
        msg: M,
    ) -> Result<UntypedMessage, crate::Error> {
        match self {
            RemoteRoleStyle::Counterpart => msg.to_untyped_message(),
            RemoteRoleStyle::Successor => SuccessorMessage {
                message: msg,
                meta: None,
            }
            .to_untyped_message(),
        }
    }

    pub(crate) async fn handle_incoming_message<R: JrRole>(
        &self,
        message_cx: MessageCx,
        connection_cx: JrConnectionCx<R>,
        handle_message: impl AsyncFnOnce(
            MessageCx,
            JrConnectionCx<R>,
        ) -> Result<Handled<MessageCx>, crate::Error>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        tracing::trace!(
            ?self,
            method = %message_cx.method(),
            role = std::any::type_name::<R>(),
            "handle_incoming_message: enter"
        );
        match self {
            RemoteRoleStyle::Counterpart => {
                tracing::trace!("handle_incoming_message: Counterpart style, passing through");
                return handle_message(message_cx, connection_cx).await;
            }
            RemoteRoleStyle::Successor => (),
        }

        let method = message_cx.method();
        if method != METHOD_SUCCESSOR_MESSAGE {
            tracing::trace!(
                method,
                expected = METHOD_SUCCESSOR_MESSAGE,
                "handle_incoming_message: Successor style but method doesn't match, returning Handled::No"
            );
            return Ok(Handled::No(message_cx));
        }

        tracing::trace!("handle_incoming_message: Successor style, unwrapping SuccessorMessage");
        // The outer message has method="_proxy/successor" and params containing the inner message.
        // We need to deserialize the params (not the whole message) to extract the inner UntypedMessage.
        let SuccessorMessage { message, meta } = json_cast(message_cx.message().params())?;
        let successor_message_cx = message_cx.try_map_message(|_| Ok(message))?;
        tracing::trace!(
            unwrapped_method = %successor_message_cx.method(),
            "handle_incoming_message: unwrapped to inner message"
        );
        match handle_message(successor_message_cx, connection_cx).await? {
            Handled::Yes => {
                tracing::trace!("handle_incoming_message: inner handler returned Handled::Yes");
                Ok(Handled::Yes)
            }
            Handled::No(successor_message_cx) => {
                tracing::trace!(
                    "handle_incoming_message: inner handler returned Handled::No, re-wrapping"
                );
                Ok(Handled::No(successor_message_cx.try_map_message(
                    |message| SuccessorMessage { message, meta }.to_untyped_message(),
                )?))
            }
        }
    }
}

// ============================================================================
// Endpoints - logical destinations for messages
// ============================================================================

/// A generic endpoint for untyped connections.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UntypedEndpoint;

impl JrEndpoint for UntypedEndpoint {}

/// Endpoint representing the client direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Client;

impl JrEndpoint for Client {}

/// Endpoint representing the agent direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Agent;

impl JrEndpoint for Agent {}

/// Endpoint representing the conductor direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Conductor;

impl JrEndpoint for Conductor {}

// ============================================================================
// Roles - directional connection types
// ============================================================================

/// A generic role for testing and dynamic dispatch scenarios.
///
/// `UntypedRole` can send to and receive from any endpoint without transformation.
/// This is useful for tests and scenarios where the exact role is not known
/// at compile time.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UntypedRole;

impl JrRole for UntypedRole {
    type HandlerEndpoint = UntypedEndpoint;
}

impl HasDefaultEndpoint for UntypedRole {}

impl HasEndpoint<UntypedEndpoint> for UntypedRole {
    fn remote_style(_end: UntypedEndpoint) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl UntypedRole {
    /// Create a connection builder with an untyped role.
    pub fn builder() -> JrConnectionBuilder<NullHandler<UntypedRole>> {
        JrConnectionBuilder::new(UntypedRole)
    }
}

/// A client's connection to an agent.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientToAgent;

impl JrRole for ClientToAgent {
    type HandlerEndpoint = Agent;
}

impl HasDefaultEndpoint for ClientToAgent {}

impl HasEndpoint<Agent> for ClientToAgent {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// An agent's connection to a client.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentToClient;

impl JrRole for AgentToClient {
    type HandlerEndpoint = Client;
}

impl HasDefaultEndpoint for AgentToClient {}

impl HasEndpoint<Client> for AgentToClient {
    fn remote_style(_end: Client) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// A conductor's connection to a client.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToClient;

impl JrRole for ConductorToClient {
    type HandlerEndpoint = Client;
}

impl HasDefaultEndpoint for ConductorToClient {}

impl HasEndpoint<Client> for ConductorToClient {
    fn remote_style(_end: Client) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

// When the conductor is acting as a proxy, it can also receive messages
// from the agent direction (wrapped in SuccessorMessage envelopes).
impl HasEndpoint<Agent> for ConductorToClient {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Successor
    }
}

/// A conductor's connection to a proxy.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToProxy;

impl JrRole for ConductorToProxy {
    type HandlerEndpoint = Agent;
}

impl HasDefaultEndpoint for ConductorToProxy {}

impl HasEndpoint<Agent> for ConductorToProxy {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// A conductor's connection to an agent.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToAgent;

impl JrRole for ConductorToAgent {
    type HandlerEndpoint = Agent;
}

impl HasDefaultEndpoint for ConductorToAgent {}

impl HasEndpoint<Agent> for ConductorToAgent {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// A proxy's connection to a conductor.
///
/// Proxies can send to two endpoints:
/// - `Client`: messages pass through unchanged
/// - `Agent`: messages are wrapped in a `SuccessorMessage` envelope
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProxyToConductor;

impl JrRole for ProxyToConductor {
    type HandlerEndpoint = Conductor;

    async fn default_message_handler(
        message: MessageCx,
        cx: JrConnectionCx<Self>,
    ) -> Result<(), crate::Error> {
        // Handle various special messages:
        let result = MatchMessage::new(message)
            .if_request(async |_req: InitializeRequest, request_cx| {
                request_cx.respond_with_error(crate::Error::invalid_request().with_data(format!(
                    "proxies must be initialized with `{}`",
                    METHOD_INITIALIZE_PROXY
                )))
            })
            .await
            // Initialize Proxy coming from the client -- forward to the agent but
            // convert into a regular initialize.
            .if_request(async |request: InitializeProxyRequest, request_cx| {
                let InitializeProxyRequest { initialize } = request;
                cx.send_request_to(Agent, initialize)
                    .forward_to_request_cx(request_cx)
            })
            .await
            // Incoming request from the agent -- forward to the client
            .if_request(async |agent_request: SuccessorMessage, request_cx| {
                let SuccessorMessage {
                    message: request,
                    meta: _,
                } = agent_request;
                cx.send_request_to(Client, request)
                    .forward_to_request_cx(request_cx)
            })
            .await
            // Incoming notification from the agent -- forward to the client
            .if_notification(async |agent_notif: SuccessorMessage| {
                let SuccessorMessage {
                    message: notif,
                    meta: _,
                } = agent_notif;
                cx.send_notification_to(Client, notif)
            })
            .await
            .done()?;

        match result {
            Handled::Yes => Ok(()),

            // All other messages are coming from the client, forward to the agent
            Handled::No(message) => match message {
                MessageCx::Request(request, request_cx) => cx
                    .send_request_to(Agent, request)
                    .forward_to_request_cx(request_cx),
                MessageCx::Notification(notif) => cx.send_notification_to(Agent, notif),
            },
        }
    }
}

impl HasEndpoint<Conductor> for ProxyToConductor {
    fn remote_style(_end: Conductor) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl HasEndpoint<Client> for ProxyToConductor {
    fn remote_style(_end: Client) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl HasEndpoint<Agent> for ProxyToConductor {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Successor
    }
}

// ============================================================================
// Convenience constructors for JrConnectionBuilder
// ============================================================================

impl ClientToAgent {
    /// Create a connection builder for a client talking to an agent.
    pub fn builder() -> JrConnectionBuilder<NullHandler<ClientToAgent>> {
        JrConnectionBuilder::new(ClientToAgent)
    }
}

impl AgentToClient {
    /// Create a connection builder for an agent talking to a client.
    pub fn builder() -> JrConnectionBuilder<NullHandler<AgentToClient>> {
        JrConnectionBuilder::new(AgentToClient)
    }
}

impl ProxyToConductor {
    /// Create a connection builder for a proxy talking to a conductor.
    pub fn builder() -> JrConnectionBuilder<NullHandler<ProxyToConductor>> {
        JrConnectionBuilder::new(ProxyToConductor)
    }
}

impl ConductorToProxy {
    /// Create a connection builder for a conductor talking to a proxy.
    pub fn builder() -> JrConnectionBuilder<NullHandler<ConductorToProxy>> {
        JrConnectionBuilder::new(ConductorToProxy)
    }
}

impl ConductorToAgent {
    /// Create a connection builder for a conductor talking to an agent.
    pub fn builder() -> JrConnectionBuilder<NullHandler<ConductorToAgent>> {
        JrConnectionBuilder::new(ConductorToAgent)
    }
}

impl ConductorToClient {
    /// Create a connection builder for a conductor talking to a client.
    pub fn builder() -> JrConnectionBuilder<NullHandler<ConductorToClient>> {
        JrConnectionBuilder::new(ConductorToClient)
    }
}
