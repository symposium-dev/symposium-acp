//! Role traits and Role types for ACP and related protocols.
//!
//! Roles are directional - they capture both "who I am" and "who I'm talking to".
//! For example, `ClientToAgent` is a client's connection to an agent.
//!
//! Peers are logical destinations for messages. Most links have a single
//! implicit peer, but proxies can send to multiple peers (`Client` or `Agent`).

use std::{fmt::Debug, hash::Hash};

use agent_client_protocol_schema::{NewSessionRequest, NewSessionResponse, SessionId};

use crate::{
    Handled, JrConnectionCx, JrMessage, JrMessageHandler, MessageCx, UntypedMessage,
    jsonrpc::{JrConnectionBuilder, handlers::NullHandler},
    schema::{
        InitializeProxyRequest, InitializeRequest, METHOD_INITIALIZE_PROXY,
        METHOD_SUCCESSOR_MESSAGE, SuccessorMessage,
    },
    util::{MatchMessageFrom, json_cast},
};

/// Trait for JSON-RPC connection roles.
///
/// The role determines what operations are valid on a connection and
/// provides role-specific behavior like handling unhandled messages.
#[expect(async_fn_in_trait)]
pub trait JrLink: Debug + Copy + Send + Sync + 'static + Eq + Ord + Hash + Default {
    /// The role being played by the remote peer on this link.
    ///
    /// When you use [`JrConnectionCx::send_request`] or [`JrConnectionBuilder::on_receive_request`], etc.
    /// this is the default peer that you are communicating with.
    ///
    /// But that default convenience is only permitted for links that
    /// implement [`HasDefaultPeer`]. More complex links like [`ProxyToConductor`]
    /// multiplex multiple "logical peers" over a single link. In that case, the
    /// `RemotePeer` is the [`Conductor`] that arranges messages.
    /// Because those links do not implement [`HasDefaultPeer`], users are required
    /// to explicitly specify the peer role they wish to use by
    /// calling [`JrConnectionCx::send_request_to`].
    type RemotePeer: JrRole;

    /// State maintained for connections this role.
    type State: Default;

    /// Method invoked when there is no defined message handler.
    /// If this returns `no`, an error response will be sent.
    async fn default_message_handler(
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        state: &mut Self::State,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        let _ = cx;
        let _ = state;
        Ok(Handled::No {
            message,
            retry: false,
        })
    }
}

/// A link that has a default peer for sending messages.
pub trait HasDefaultPeer: JrLink {}

/// A logical destination for messages (e.g., Client, Agent, McpServer).
pub trait JrRole: Debug + Copy + Send + Sync + 'static + Eq + Ord + Hash + Default {}

/// Declares that a link can send messages to a specific peer role.
pub trait HasPeer<Peer: JrRole>: JrLink {
    /// Returns the remote role style for sending to this peer.
    fn remote_style(peer: Peer) -> RemoteRoleStyle;
}

/// Describes how messages are transformed when sent to a remote peer.
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

    pub(crate) async fn handle_incoming_message<R: JrLink>(
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
            return Ok(Handled::No {
                message: message_cx,
                retry: false,
            });
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

            Handled::No {
                message: successor_message_cx,
                retry,
            } => {
                tracing::trace!(
                    "handle_incoming_message: inner handler returned Handled::No, re-wrapping"
                );
                Ok(Handled::No {
                    message: successor_message_cx.try_map_message(|message| {
                        SuccessorMessage { message, meta }.to_untyped_message()
                    })?,
                    retry,
                })
            }
        }
    }
}

// ============================================================================
// Roles - logical destinations for messages
// ============================================================================

/// A generic role for untyped connections.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UntypedRole;

impl JrRole for UntypedRole {}

/// Role representing the client direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Client;

impl JrRole for Client {}

/// Role representing the agent direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Agent;

impl JrRole for Agent {}

/// Role representing the conductor direction.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Conductor;

impl JrRole for Conductor {}

// ============================================================================
// Links - directional connection types
// ============================================================================

/// A generic link for testing scenarios.
///
/// `UntypedLink` can send to and receive from any peer without transformation.
/// This is useful for tests and scenarios where the exact role is not known
/// at compile time.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UntypedLink;

impl JrLink for UntypedLink {
    type RemotePeer = UntypedRole;

    type State = ();
}

impl HasDefaultPeer for UntypedLink {}

impl HasPeer<UntypedRole> for UntypedLink {
    fn remote_style(_end: UntypedRole) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl UntypedLink {
    /// Create a connection builder with an untyped role.
    pub fn builder() -> JrConnectionBuilder<NullHandler<UntypedLink>> {
        JrConnectionBuilder::new(UntypedLink)
    }
}

/// A client's connection to an agent.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientToAgent;

impl JrLink for ClientToAgent {
    type RemotePeer = Agent;

    type State = ();

    async fn default_message_handler(
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        _state: &mut (),
    ) -> Result<Handled<MessageCx>, crate::Error> {
        MatchMessageFrom::new(message, &cx)
            .if_message_from(Agent, async |message: MessageCx| {
                // Subtle: messages that have a session-id field
                // should be captured by a dynamic message handler
                // for that session -- but there is a race condition
                // between the dynamic handler being added and
                // possible updates. Therefore, we "retry" all such
                // messages, so that they will be resent as new handlers
                // are added.
                let retry = message.has_session_id();
                Ok(Handled::No { message, retry })
            })
            .await
            .done()
    }
}

impl HasDefaultPeer for ClientToAgent {}

impl HasPeer<Agent> for ClientToAgent {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// An agent's connection to a client.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentToClient;

impl JrLink for AgentToClient {
    type RemotePeer = Client;
    type State = ();
}

impl HasDefaultPeer for AgentToClient {}

impl HasPeer<Client> for AgentToClient {
    fn remote_style(_end: Client) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// A conductor's connection to a client.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToClient;

impl JrLink for ConductorToClient {
    type RemotePeer = Client;
    type State = ();
}

impl HasPeer<Client> for ConductorToClient {
    fn remote_style(_end: Client) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

// When the conductor is acting as a proxy, it can also receive messages
// from the agent direction (wrapped in SuccessorMessage envelopes).
impl HasPeer<Agent> for ConductorToClient {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Successor
    }
}

/// A conductor's connection to a proxy.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToProxy;

impl JrLink for ConductorToProxy {
    type RemotePeer = Agent;
    type State = ();
}

impl HasDefaultPeer for ConductorToProxy {}

impl HasPeer<Agent> for ConductorToProxy {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// A conductor's connection to an agent.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToAgent;

impl JrLink for ConductorToAgent {
    type RemotePeer = Agent;
    type State = ();
}

impl HasDefaultPeer for ConductorToAgent {}

impl HasPeer<Agent> for ConductorToAgent {
    fn remote_style(_end: Agent) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

/// A proxy's connection to a conductor.
///
/// Proxies can send to two peers:
/// - `Client`: messages pass through unchanged
/// - `Agent`: messages are wrapped in a `SuccessorMessage` envelope
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProxyToConductor;

/// Internal state for handling proxied messages.
#[derive(Default)]
pub struct ProxyToConductorState {}

impl JrLink for ProxyToConductor {
    type RemotePeer = Conductor;
    type State = ProxyToConductorState;

    async fn default_message_handler(
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        _state: &mut Self::State,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        // Handle various special messages:
        let result = MatchMessageFrom::new(message, &cx)
            .if_request_from(Client, async |_req: InitializeRequest, request_cx| {
                request_cx.respond_with_error(crate::Error::invalid_request().with_data(format!(
                    "proxies must be initialized with `{}`",
                    METHOD_INITIALIZE_PROXY
                )))
            })
            .await
            // Initialize Proxy coming from the client -- forward to the agent but
            // convert into a regular initialize.
            .if_request_from(
                Client,
                async |request: InitializeProxyRequest, request_cx| {
                    let InitializeProxyRequest { initialize } = request;
                    cx.send_request_to(Agent, initialize)
                        .forward_to_request_cx(request_cx)
                },
            )
            .await
            // Incoming request from the agent -- forward to the client
            .if_request_from(
                Conductor,
                async |agent_request: SuccessorMessage, request_cx| {
                    let SuccessorMessage {
                        message: request,
                        meta: _,
                    } = agent_request;
                    cx.send_request_to(Client, request)
                        .forward_to_request_cx(request_cx)
                },
            )
            .await
            // New session coming from the client -- proxy to the agent
            // and add a dynamic handler for that
            // session-id.
            .if_request_from(Client, async |request: NewSessionRequest, request_cx| {
                cx.send_request_to(Agent, request).on_receiving_result({
                    let cx = cx.clone();
                    async move |result| {
                        if let Ok(NewSessionResponse { session_id, .. }) = &result {
                            cx.add_dynamic_handler(ProxySessionMessages::new(session_id.clone()))?
                                .run_indefinitely();
                        }
                        request_cx.respond_with_result(result)
                    }
                })
            })
            .await
            // Incoming notification from the agent -- forward to the client
            .if_notification_from(Conductor, async |agent_notif: SuccessorMessage| {
                let SuccessorMessage {
                    message: notif,
                    meta: _,
                } = agent_notif;
                cx.send_notification_to(Client, notif)
            })
            .await
            .done()?;

        match result {
            Handled::Yes => Ok(Handled::Yes),

            // If we got a retry, pass it up to be retried.
            Handled::No {
                message,
                retry: true,
            } => Ok(Handled::No {
                message,
                retry: true,
            }),

            // All other messages are coming from the client, forward to the agent
            Handled::No {
                message,
                retry: false,
            } => match message {
                MessageCx::Request(request, request_cx) => {
                    cx.send_request_to(Agent, request)
                        .forward_to_request_cx(request_cx)?;
                    Ok(Handled::Yes)
                }
                MessageCx::Notification(notif) => {
                    cx.send_notification_to(Agent, notif)?;
                    Ok(Handled::Yes)
                }
            },
        }
    }
}

/// Dynamic handler that proxies session messages from Agent to Client.
///
/// This is used internally to handle session message routing after a
/// `session.new` request has been forwarded.
pub(crate) struct ProxySessionMessages<Link> {
    session_id: SessionId,
    _marker: std::marker::PhantomData<Link>,
}

impl<Link> ProxySessionMessages<Link> {
    /// Create a new proxy handler for the given session.
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<Link: JrLink> JrMessageHandler for ProxySessionMessages<Link>
where
    Link: HasPeer<Agent> + HasPeer<Client>,
{
    type Link = Link;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Self::Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        MatchMessageFrom::new(message, &cx)
            .if_message_from(Agent, async |message| {
                // If this is for our session-id, proxy it to the client.
                if let Some(session_id) = message.get_session_id()? {
                    if session_id == self.session_id {
                        cx.send_proxied_message_to(Client, message)?;
                        return Ok(Handled::Yes);
                    }
                }

                // Otherwise, leave it alone.
                Ok(Handled::No {
                    message,
                    retry: false,
                })
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!("ProxySessionMessages({})", self.session_id)
    }
}

impl HasPeer<Conductor> for ProxyToConductor {
    fn remote_style(_end: Conductor) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl HasPeer<Client> for ProxyToConductor {
    fn remote_style(_end: Client) -> RemoteRoleStyle {
        RemoteRoleStyle::Counterpart
    }
}

impl HasPeer<Agent> for ProxyToConductor {
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
