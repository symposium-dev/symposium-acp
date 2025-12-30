//! Link types for ACP and related protocols.
//!
//! Links are directional - they capture the relationship between two sides.
//! For example, `ClientToAgent` is a client's connection to an agent.

use std::{fmt::Debug, hash::Hash};

use agent_client_protocol_schema::{NewSessionRequest, NewSessionResponse, SessionId};

use crate::{
    Handled, JrConnectionCx, JrMessage, JrMessageHandler, MessageCx, UntypedMessage,
    jsonrpc::{JrConnectionBuilder, handlers::NullHandler},
    peer::{AgentPeer, ClientPeer, ConductorPeer, JrPeer, ProxyPeer, UntypedPeer},
    schema::{
        InitializeProxyRequest, InitializeRequest, METHOD_INITIALIZE_PROXY,
        METHOD_SUCCESSOR_MESSAGE, SuccessorMessage,
    },
    util::{MatchMessageFrom, json_cast},
};

/// Trait for JSON-RPC connection links.
///
/// The link specifies two communicating peers, e.g., [`ClientToAgent`].
/// The link determines what operations are valid on a connection and
/// provides link-specific behavior like handling unhandled messages.
pub trait JrLink: Debug + Copy + Send + Sync + 'static + Eq + Ord + Hash + Default {
    /// Create a new connection builder for this link.
    fn builder() -> JrConnectionBuilder<NullHandler<Self>> {
        JrConnectionBuilder::new(Self::default())
    }

    /// The link type that connects to this link.
    ///
    /// For example, `ConnectsTo` for [`ClientToAgent`] is [`AgentToClient`].
    ///
    /// This is used by `Component<L>` to express the relationship:
    /// a component that implements `Component<L>` can serve as the
    /// transport for a connection using link `L::ConnectsTo`.
    type ConnectsTo: JrLink;

    /// State maintained for connections this link.
    type State: Default + Send;

    /// Method invoked when there is no defined message handler.
    /// If this returns `no`, an error response will be sent.
    fn default_message_handler(
        message: MessageCx,
        #[expect(unused_variables)] cx: JrConnectionCx<Self>,
        #[expect(unused_variables)] state: &mut Self::State,
    ) -> impl Future<Output = Result<Handled<MessageCx>, crate::Error>> + Send {
        async move {
            Ok(Handled::No {
                message,
                retry: false,
            })
        }
    }
}

/// A link that has a default peer for sending messages.
///
/// Links like [`ProxyToConductor`] multiplex multiple "logical peers" over a single link
/// and do not implement this trait. Users of those links must explicitly specify
/// the peer by calling [`JrConnectionCx::send_request_to`].
pub trait HasDefaultPeer: JrLink + HasPeer<Self::DefaultPeer> {
    /// The default peer for this link.
    ///
    /// When you use [`JrConnectionCx::send_request`] or [`JrConnectionBuilder::on_receive_request`], etc.
    /// this is the peer that you are communicating with.
    type DefaultPeer: JrPeer;
}

/// Declares that a link can send messages to a specific peer.
pub trait HasPeer<Peer: JrPeer>: JrLink {
    /// Returns the remote style for sending to this peer.
    fn remote_style(peer: Peer) -> RemoteStyle;
}

/// Describes how messages are transformed when sent to a remote peer.
#[derive(Debug)]
#[non_exhaustive]
pub enum RemoteStyle {
    /// Pass each message through exactly as it is.
    Counterpart,

    /// Wrap messages in a [`SuccessorMessage`] envelope.
    Successor,
}

impl RemoteStyle {
    pub(crate) fn transform_outgoing_message<M: JrMessage>(
        &self,
        msg: M,
    ) -> Result<UntypedMessage, crate::Error> {
        match self {
            RemoteStyle::Counterpart => msg.to_untyped_message(),
            RemoteStyle::Successor => SuccessorMessage {
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
            RemoteStyle::Counterpart => {
                tracing::trace!("handle_incoming_message: Counterpart style, passing through");
                return handle_message(message_cx, connection_cx).await;
            }
            RemoteStyle::Successor => (),
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
// Links - directional connection types
// ============================================================================

/// A generic link for testing scenarios.
///
/// `UntypedLink` can send to and receive from any peer without transformation.
/// This is useful for tests but generally shouldn't really be used in production code.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UntypedLink;

impl JrLink for UntypedLink {
    type ConnectsTo = UntypedLink;
    type State = ();
}

impl HasDefaultPeer for UntypedLink {
    type DefaultPeer = UntypedPeer;
}

impl HasPeer<UntypedPeer> for UntypedLink {
    fn remote_style(_end: UntypedPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

impl UntypedLink {
    /// Create a connection builder with an untyped link.
    pub fn builder() -> JrConnectionBuilder<NullHandler<UntypedLink>> {
        JrConnectionBuilder::new(UntypedLink)
    }
}

/// A client connecting to its agent. Use this when attempting to issue prompts
/// or requests to an agent using the SDK.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientToAgent;

impl JrLink for ClientToAgent {
    type ConnectsTo = AgentToClient;
    type State = ();

    async fn default_message_handler(
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        _state: &mut (),
    ) -> Result<Handled<MessageCx>, crate::Error> {
        MatchMessageFrom::new(message, &cx)
            .if_message_from(AgentPeer, async |message: MessageCx| {
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

impl HasDefaultPeer for ClientToAgent {
    type DefaultPeer = AgentPeer;
}

impl HasPeer<AgentPeer> for ClientToAgent {
    fn remote_style(_end: AgentPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

/// An agent connecting to its client. This is used when implementing agents.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentToClient;

impl JrLink for AgentToClient {
    type ConnectsTo = ClientToAgent;
    type State = ();
}

impl HasDefaultPeer for AgentToClient {
    type DefaultPeer = ClientPeer;
}

impl HasPeer<ClientPeer> for AgentToClient {
    fn remote_style(_end: ClientPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

/// A conductor's connection to a client.
/// The conductor is acting as a composite agent that combines
/// some number of proxies with a final agent.
///
/// This is only meant to be used as a link type of the conductor itself;
/// if you find yourself using it for something else you are probably
/// doing something wrong.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToClient;

impl JrLink for ConductorToClient {
    type ConnectsTo = ClientToAgent; // Client talks to conductor as if it were an agent
    type State = ();
}

impl HasPeer<ClientPeer> for ConductorToClient {
    fn remote_style(_end: ClientPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

/// A conductor's connection to another conductor.
/// The inner/local conductor is acting as a proxy
/// and the outer/local conductor is acting as its conductor.
/// This is used creating a proxy that uses a conductor to arrange many proxies.
///
/// This is only meant to be used as a link type of the conductor itself;
/// if you find yourself using it for something else you are probably
/// doing something wrong.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToConductor;

impl JrLink for ConductorToConductor {
    /// From the (remote) conductor's perspective, thie (local) conductor is a proxy
    type ConnectsTo = ConductorToProxy;
    type State = ();
}

impl HasPeer<AgentPeer> for ConductorToConductor {
    fn remote_style(_end: AgentPeer) -> RemoteStyle {
        RemoteStyle::Successor
    }
}

impl HasPeer<ClientPeer> for ConductorToConductor {
    fn remote_style(_end: ClientPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

/// A conductor's connection to a proxy.
///
/// This is only meant to be used by the conductor;
/// if you find yourself using it for something else you are probably
/// doing something wrong.
///
/// This is very similar to a [`ClientToAgent`] connection
/// but conductors and proxies also exchange [`SuccessorMessage`] messages
/// that indicate communication with the proxy's successor,
/// which may be another proxy or an agent.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToProxy;

impl JrLink for ConductorToProxy {
    type ConnectsTo = ProxyToConductor;
    type State = ();
}

impl HasDefaultPeer for ConductorToProxy {
    type DefaultPeer = ProxyPeer;
}

impl HasPeer<ProxyPeer> for ConductorToProxy {
    fn remote_style(_end: ProxyPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

impl HasPeer<AgentPeer> for ConductorToProxy {
    fn remote_style(_end: AgentPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

/// A conductor's connection to its final agent.
///
/// This is only meant to be used by the conductor;
/// if you find yourself using it for something else you are probably
/// doing something wrong.
///
/// This is the same as a [`ClientToAgent`] connection except that
/// the default message handling is different; the conductor doesn't use
/// dynamic handlers for individual sessions and therefore the default
/// message handling simply doesn't include any "retry" logic.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConductorToAgent;

impl JrLink for ConductorToAgent {
    /// From the (remote) agent's perspective, the conductor is acting as any other client
    type ConnectsTo = AgentToClient;
    type State = ();
}

impl HasDefaultPeer for ConductorToAgent {
    type DefaultPeer = AgentPeer;
}

impl HasPeer<AgentPeer> for ConductorToAgent {
    fn remote_style(_end: AgentPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
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
    type ConnectsTo = ConductorToProxy;
    type State = ProxyToConductorState;

    async fn default_message_handler(
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        _state: &mut Self::State,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        // Handle various special messages:
        let result = MatchMessageFrom::new(message, &cx)
            .if_request_from(ClientPeer, async |_req: InitializeRequest, request_cx| {
                request_cx.respond_with_error(crate::Error::invalid_request().data(format!(
                    "proxies must be initialized with `{}`",
                    METHOD_INITIALIZE_PROXY
                )))
            })
            .await
            // Initialize Proxy coming from the client -- forward to the agent but
            // convert into a regular initialize.
            .if_request_from(
                ClientPeer,
                async |request: InitializeProxyRequest, request_cx| {
                    let InitializeProxyRequest { initialize } = request;
                    cx.send_request_to(AgentPeer, initialize)
                        .forward_to_request_cx(request_cx)
                },
            )
            .await
            // Incoming request from the agent -- forward to the client
            .if_request_from(
                ConductorPeer,
                async |agent_request: SuccessorMessage, request_cx| {
                    let SuccessorMessage {
                        message: request,
                        meta: _,
                    } = agent_request;
                    cx.send_request_to(ClientPeer, request)
                        .forward_to_request_cx(request_cx)
                },
            )
            .await
            // New session coming from the client -- proxy to the agent
            // and add a dynamic handler for that
            // session-id.
            .if_request_from(
                ClientPeer,
                async |request: NewSessionRequest, request_cx| {
                    cx.send_request_to(AgentPeer, request).on_receiving_result({
                        let cx = cx.clone();
                        async move |result| {
                            if let Ok(NewSessionResponse { session_id, .. }) = &result {
                                cx.add_dynamic_handler(ProxySessionMessages::new(
                                    session_id.clone(),
                                ))?
                                .run_indefinitely();
                            }
                            request_cx.respond_with_result(result)
                        }
                    })
                },
            )
            .await
            // Incoming notification from the agent -- forward to the client
            .if_notification_from(ConductorPeer, async |agent_notif: SuccessorMessage| {
                let SuccessorMessage {
                    message: notif,
                    meta: _,
                } = agent_notif;
                cx.send_notification_to(ClientPeer, notif)
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
                    cx.send_request_to(AgentPeer, request)
                        .forward_to_request_cx(request_cx)?;
                    Ok(Handled::Yes)
                }
                MessageCx::Notification(notif) => {
                    cx.send_notification_to(AgentPeer, notif)?;
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
    Link: HasPeer<AgentPeer> + HasPeer<ClientPeer>,
{
    type Link = Link;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Self::Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        MatchMessageFrom::new(message, &cx)
            .if_message_from(AgentPeer, async |message| {
                // If this is for our session-id, proxy it to the client.
                if let Some(session_id) = message.get_session_id()? {
                    if session_id == self.session_id {
                        cx.send_proxied_message_to(ClientPeer, message)?;
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

impl HasPeer<ConductorPeer> for ProxyToConductor {
    fn remote_style(_end: ConductorPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

impl HasPeer<ClientPeer> for ProxyToConductor {
    fn remote_style(_end: ClientPeer) -> RemoteStyle {
        RemoteStyle::Counterpart
    }
}

impl HasPeer<AgentPeer> for ProxyToConductor {
    fn remote_style(_end: AgentPeer) -> RemoteStyle {
        RemoteStyle::Successor
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
