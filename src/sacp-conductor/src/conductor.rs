//! # Conductor: SACP Proxy Chain Orchestrator
//!
//! This module implements the Conductor conductor, which orchestrates a chain of
//! proxy components that sit between an editor and an agent, transforming the
//! Agent-Client Protocol (ACP) stream bidirectionally.
//!
//! ## Architecture Overview
//!
//! The conductor builds and manages a chain of components:
//!
//! ```text
//! Editor <-ACP-> [Component 0] <-ACP-> [Component 1] <-ACP-> ... <-ACP-> Agent
//! ```
//!
//! Each component receives ACP messages, can transform them, and forwards them
//! to the next component in the chain. The conductor:
//!
//! 1. Spawns each component as a subprocess
//! 2. Establishes bidirectional JSON-RPC connections with each component
//! 3. Routes messages between editor, components, and agent
//! 4. Distinguishes proxy vs agent components via distinct request types
//!
//! ## Recursive Chain Building
//!
//! The chain is built recursively through the `_proxy/successor/*` protocol:
//!
//! 1. Editor connects to Component 0 via the conductor
//! 2. When Component 0 wants to communicate with its successor, it sends
//!    requests/notifications with method prefix `_proxy/successor/`
//! 3. The conductor intercepts these messages, strips the prefix, and forwards
//!    to Component 1
//! 4. Component 1 does the same for Component 2, and so on
//! 5. The last component talks directly to the agent (no `_proxy/successor/` prefix)
//!
//! This allows each component to be written as if it's talking to a single successor,
//! without knowing about the full chain.
//!
//! ## Proxy vs Agent Initialization
//!
//! Components discover whether they're a proxy or agent via the initialization request they receive:
//!
//! - **Proxy components**: Receive `InitializeProxyRequest` (`_proxy/initialize` method)
//! - **Agent component**: Receives standard `InitializeRequest` (`initialize` method)
//!
//! The conductor sends `InitializeProxyRequest` to all proxy components in the chain,
//! and `InitializeRequest` only to the final agent component. This allows proxies to
//! know they should forward messages to a successor, while agents know they are the
//! terminal component
//!
//! ## Message Routing
//!
//! The conductor runs an event loop processing messages from:
//!
//! - **Editor to first component**: Standard ACP messages
//! - **Component to successor**: Via `_proxy/successor/*` prefix
//! - **Component responses**: Via futures channels back to requesters
//!
//! The message flow ensures bidirectional communication while maintaining the
//! abstraction that each component only knows about its immediate successor.
//!
//! ## Lazy Component Initialization
//!
//! Components are instantiated lazily when the first `initialize` request is received
//! from the editor. This enables dynamic proxy chain construction based on client capabilities.
//!
//! ### Simple Usage
//!
//! Pass a Vec of components that implement `Component`:
//!
//! ```ignore
//! let conductor = Conductor::new(
//!     "my-conductor",
//!     vec![proxy1, proxy2, agent],
//!     None,
//! );
//! ```
//!
//! All components are spawned in order when the editor sends the first `initialize` request.
//!
//! ### Dynamic Component Selection
//!
//! Pass a closure to examine the `InitializeRequest` and dynamically construct the chain:
//!
//! ```ignore
//! let conductor = Conductor::new(
//!     "my-conductor",
//!     |cx, conductor_tx, init_req| async move {
//!         // Examine capabilities
//!         let needs_auth = has_auth_capability(&init_req);
//!
//!         let mut components = Vec::new();
//!         if needs_auth {
//!             components.push(spawn_auth_proxy(&cx, &conductor_tx)?);
//!         }
//!         components.push(spawn_agent(&cx, &conductor_tx)?);
//!
//!         // Return (potentially modified) request and component list
//!         Ok((init_req, components))
//!     },
//!     None,
//! );
//! ```
//!
//! The closure receives:
//! - `cx: &JrConnectionCx` - Connection context for spawning components
//! - `conductor_tx: &mpsc::Sender<ConductorMessage>` - Channel for message routing
//! - `init_req: InitializeRequest` - The Initialize request from the editor
//!
//! And returns:
//! - Modified `InitializeRequest` to forward downstream
//! - `Vec<JrConnectionCx>` of spawned components

use std::{collections::HashMap, sync::Arc};

use futures::{
    SinkExt, StreamExt,
    channel::mpsc::{self},
};
use sacp::{
    AgentPeer, BoxFuture, ClientPeer, Component, Error, HasPeer, JrMessage, JrResponsePayload as _,
    MessageCx,
    link::{
        AgentToClient, ConductorToAgent, ConductorToClient, ConductorToConductor, ConductorToProxy,
        ProxyToConductor,
    },
    schema::METHOD_MCP_MESSAGE,
    util::MatchMessage,
};
use sacp::{
    Handled,
    schema::{
        McpConnectRequest, McpConnectResponse, McpDisconnectNotification, McpOverAcpMessage,
        SuccessorMessage,
    },
};
use sacp::{
    JrConnectionBuilder, JrConnectionCx, JrLink, JrNotification, JrRequest, JrResponse,
    UntypedMessage,
};
use sacp::{
    JrMessageHandler, JrResponder,
    schema::{InitializeProxyRequest, InitializeRequest, NewSessionRequest},
    util::MatchMessageFrom,
};
use tracing::{debug, info};

use crate::conductor::mcp_bridge::{
    McpBridgeConnection, McpBridgeConnectionActor, McpBridgeListeners,
};

mod mcp_bridge;

/// The conductor manages the proxy chain lifecycle and message routing.
///
/// It maintains connections to all components in the chain and routes messages
/// bidirectionally between the editor, components, and agent.
///
pub struct Conductor<Link: ConductorLink> {
    name: String,
    instantiator: Link::Instantiator,
    mcp_bridge_mode: crate::McpBridgeMode,
    trace_writer: Option<crate::trace::TraceWriter>,
    link: Link,
}

impl<Link: ConductorLink> Conductor<Link> {
    pub fn new(
        link: Link,
        name: impl ToString,
        instantiator: Link::Instantiator,
        mcp_bridge_mode: crate::McpBridgeMode,
    ) -> Self {
        Conductor {
            name: name.to_string(),
            instantiator,
            mcp_bridge_mode,
            trace_writer: None,
            link,
        }
    }
}

impl Conductor<ConductorToClient> {
    /// Create a conductor in agent mode (the last component is an agent).
    pub fn new_agent(
        name: impl ToString,
        instantiator: impl InstantiateProxiesAndAgent + 'static,
        mcp_bridge_mode: crate::McpBridgeMode,
    ) -> Self {
        Conductor::new(
            ConductorToClient,
            name,
            Box::new(instantiator),
            mcp_bridge_mode,
        )
    }
}

impl Conductor<ConductorToConductor> {
    /// Create a conductor in proxy mode (forwards to another conductor).
    pub fn new_proxy(
        name: impl ToString,
        instantiator: impl InstantiateProxies + 'static,
        mcp_bridge_mode: crate::McpBridgeMode,
    ) -> Self {
        Conductor::new(
            ConductorToConductor,
            name,
            Box::new(instantiator),
            mcp_bridge_mode,
        )
    }
}

impl<Link: ConductorLink> Conductor<Link> {
    /// Enable trace logging to a custom destination.
    ///
    /// Use `sacp-trace-viewer` to view the trace as an interactive sequence diagram.
    pub fn trace_to(mut self, dest: impl crate::trace::WriteEvent) -> Self {
        self.trace_writer = Some(crate::trace::TraceWriter::new(dest));
        self
    }

    /// Enable trace logging to a file path.
    ///
    /// Events will be written as newline-delimited JSON (`.jsons` format).
    /// Use `sacp-trace-viewer` to view the trace as an interactive sequence diagram.
    pub fn trace_to_path(mut self, path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        self.trace_writer = Some(crate::trace::TraceWriter::from_path(path)?);
        Ok(self)
    }

    /// Enable trace logging with an existing TraceWriter.
    pub fn with_trace_writer(mut self, writer: crate::trace::TraceWriter) -> Self {
        self.trace_writer = Some(writer);
        self
    }

    pub fn into_connection_builder(
        self,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = Link>, impl JrResponder<Link>> {
        let (conductor_tx, conductor_rx) = mpsc::channel(128 /* chosen arbitrarily */);

        let responder = ConductorResponder {
            conductor_rx,
            conductor_tx: conductor_tx.clone(),
            instantiator: Some(self.instantiator),
            bridge_listeners: Default::default(),
            bridge_connections: Default::default(),
            mcp_bridge_mode: self.mcp_bridge_mode,
            proxies: Default::default(),
            successor: Arc::new(sacp::util::internal_error("successor not initialized")),
            trace_writer: self.trace_writer,
            link: self.link,
        };

        JrConnectionBuilder::new_with(ConductorMessageHandler {
            conductor_tx,
            link: self.link,
        })
        .name(self.name)
        .with_responder(responder)
    }

    /// Convenience method to run the conductor with a transport.
    ///
    /// This is equivalent to:
    /// ```ignore
    /// conductor.into_connection_builder()
    ///     .connect_to(transport)
    ///     .serve()
    ///     .await
    /// ```
    pub async fn run(
        self,
        transport: impl Component<Link::ConnectsTo> + 'static,
    ) -> Result<(), sacp::Error> {
        self.into_connection_builder()
            .connect_to(transport)?
            .serve()
            .await
    }

    async fn incoming_message_from_client(
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
        message: MessageCx,
    ) -> Result<(), sacp::Error> {
        conductor_tx
            .send(ConductorMessage::LeftToRight {
                target_component_index: 0,
                message,
            })
            .await
            .map_err(sacp::util::internal_error)
    }

    async fn incoming_message_from_agent(
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
        message: MessageCx,
    ) -> Result<(), sacp::Error> {
        conductor_tx
            .send(ConductorMessage::RightToLeft {
                source_component_index: SourceComponentIndex::Successor,
                message,
            })
            .await
            .map_err(sacp::util::internal_error)
    }
}

impl<Link: ConductorLink> Component<Link::Speaks> for Conductor<Link> {
    async fn serve(
        self,
        client: impl sacp::Component<Link::ConnectsTo>,
    ) -> Result<(), sacp::Error> {
        self.run(client).await
    }
}

struct ConductorMessageHandler<Link: ConductorLink> {
    conductor_tx: mpsc::Sender<ConductorMessage>,
    link: Link,
}

impl<Link: ConductorLink> JrMessageHandler for ConductorMessageHandler<Link> {
    type Link = Link;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        cx: sacp::JrConnectionCx<Link>,
    ) -> Result<sacp::Handled<MessageCx>, sacp::Error> {
        self.link
            .handle_message(message, cx, &mut self.conductor_tx)
            .await
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ConductorMessageHandler"
    }
}

/// The conductor manages the proxy chain lifecycle and message routing.
///
/// It maintains connections to all components in the chain and routes messages
/// bidirectionally between the editor, components, and agent.
///
pub struct ConductorResponder<Link>
where
    Link: ConductorLink,
{
    conductor_rx: mpsc::Receiver<ConductorMessage>,

    conductor_tx: mpsc::Sender<ConductorMessage>,

    /// Manages the TCP listeners for MCP connections that will be proxied over ACP.
    bridge_listeners: McpBridgeListeners,

    /// Manages active connections to MCP clients.
    bridge_connections: HashMap<String, McpBridgeConnection>,

    /// The instantiator for lazy initialization.
    /// Set to None after components are instantiated.
    instantiator: Option<Link::Instantiator>,

    /// The chain of proxies before the agent (if any).
    ///
    /// Populated lazily when the first Initialize request is received.
    proxies: Vec<JrConnectionCx<ConductorToProxy>>,

    /// If the conductor is operating in agent mode, this will direct messages to the agent.
    /// If the conductor is operating in proxy mode, this will direct messages to the successor.
    /// Populated lazily when the first Initialize request is received; the initial value just returns errors.
    successor: Arc<dyn ConductorSuccessor<Link>>,

    /// Mode for the MCP bridge (determines how to spawn bridge processes).
    mcp_bridge_mode: crate::McpBridgeMode,

    /// Optional trace writer for sequence diagram visualization.
    trace_writer: Option<crate::trace::TraceWriter>,

    /// Defines what sort of link we have
    link: Link,
}

impl<Link> JrResponder<Link> for ConductorResponder<Link>
where
    Link: ConductorLink,
{
    async fn run(mut self, cx: JrConnectionCx<Link>) -> Result<(), sacp::Error> {
        // Components are now spawned lazily in forward_initialize_request
        // when the first Initialize request is received.

        // This is the "central actor" of the conductor. Most other things forward messages
        // via `conductor_tx` into this loop. This lets us serialize the conductor's activity.
        while let Some(message) = self.conductor_rx.next().await {
            self.handle_conductor_message(cx.clone(), message).await?;
        }
        Ok(())
    }
}

impl<Link> ConductorResponder<Link>
where
    Link: ConductorLink,
{
    /// Convert a component index to a trace-friendly name.
    fn component_name(&self, index: usize) -> String {
        if index == self.proxies.len() {
            "agent".to_string()
        } else {
            format!("proxy:{}", index)
        }
    }

    /// Convert a source component index to a trace-friendly name.
    fn source_component_name(&self, index: SourceComponentIndex) -> String {
        match index {
            SourceComponentIndex::Successor => "agent".to_string(), // In proxy mode, successor is effectively the agent
            SourceComponentIndex::Proxy(i) => self.component_name(i),
        }
    }

    /// Extract the protocol and idealized method/params from a message.
    ///
    /// For MCP-over-ACP messages, this extracts the inner MCP message.
    /// For regular ACP messages, returns the message as-is.
    fn extract_trace_info<R: sacp::JrRequest, N: sacp::JrNotification>(
        message: &MessageCx<R, N>,
    ) -> Result<(crate::trace::Protocol, String, serde_json::Value), sacp::Error> {
        match message {
            MessageCx::Request(request, _) => {
                let untyped = request.to_untyped_message()?;

                // Try to parse as MCP-over-ACP request
                if <McpOverAcpMessage<UntypedMessage>>::matches_method(&untyped.method) {
                    if let Ok(mcp_req) = <McpOverAcpMessage<UntypedMessage>>::parse_message(
                        &untyped.method,
                        &untyped.params,
                    ) {
                        return Ok((
                            crate::trace::Protocol::Mcp,
                            mcp_req.message.method,
                            mcp_req.message.params,
                        ));
                    }
                }

                // Regular ACP request
                Ok((crate::trace::Protocol::Acp, untyped.method, untyped.params))
            }
            MessageCx::Notification(notification) => {
                let untyped = notification.to_untyped_message()?;

                // Try to parse as MCP-over-ACP notification
                if <McpOverAcpMessage<UntypedMessage>>::matches_method(&untyped.method) {
                    if let Ok(mcp_notif) = <McpOverAcpMessage<UntypedMessage>>::parse_message(
                        &untyped.method,
                        &untyped.params,
                    ) {
                        return Ok((
                            crate::trace::Protocol::Mcp,
                            mcp_notif.message.method,
                            mcp_notif.message.params,
                        ));
                    }
                }

                // Regular ACP notification
                Ok((crate::trace::Protocol::Acp, untyped.method, untyped.params))
            }
            MessageCx::Response(payload, response_cx) => {
                let method = response_cx.method();
                let protocol = match method {
                    METHOD_MCP_MESSAGE => crate::trace::Protocol::Mcp,
                    _ => crate::trace::Protocol::Acp,
                };
                let value: serde_json::Value = match payload {
                    Ok(v) => v.clone().into_json(method)?,
                    Err(e) => serde_json::to_value(e)?,
                };
                return Ok((protocol, method.to_string(), value));
            }
        }
    }

    /// Trace a client-to-agent message (request or notification).
    fn trace_client_to_agent<R: sacp::JrRequest, N: sacp::JrNotification>(
        &mut self,
        target_index: usize,
        message: &MessageCx<R, N>,
    ) -> Result<(), sacp::Error> {
        if self.trace_writer.is_none() {
            return Ok(());
        }

        let from = if target_index == 0 {
            "client".to_string()
        } else {
            self.component_name(target_index - 1)
        };
        let to = self.component_name(target_index);

        let (protocol, method, params) = Self::extract_trace_info(message)?;

        tracing::debug!(
            id_key = ?message.id(),
            ?from,
            ?to,
            ?method,
            ?params,
            "trace_client_to_agent"
        );

        let writer = self.trace_writer.as_mut().unwrap();
        match message {
            MessageCx::Request(_, request_cx) => {
                writer.request(protocol, from, to, request_cx.id(), &method, None, params);
            }
            MessageCx::Notification(_) => {
                writer.notification(protocol, from, to, &method, None, params);
            }
            MessageCx::Response(result, response_cx) => {
                writer.response(from, to, response_cx.id(), result.is_err(), params);
            }
        }
        Ok(())
    }

    /// Trace an agent-to-client message (request or notification).
    fn trace_agent_to_client<R: sacp::JrRequest, N: sacp::JrNotification>(
        &mut self,
        source_index: SourceComponentIndex,
        message: &MessageCx<R, N>,
    ) -> Result<(), sacp::Error> {
        if self.trace_writer.is_none() {
            return Ok(());
        }

        let from = self.source_component_name(source_index);
        let to = match source_index {
            SourceComponentIndex::Successor => {
                if self.proxies.is_empty() {
                    "client".to_string()
                } else {
                    self.component_name(self.proxies.len() - 1)
                }
            }
            SourceComponentIndex::Proxy(0) => "client".to_string(),
            SourceComponentIndex::Proxy(i) => self.component_name(i - 1),
        };

        let (protocol, method, params) = Self::extract_trace_info(message)?;

        tracing::debug!(
            id_key = ?message.id(),
            ?from,
            ?to,
            ?method,
            ?params,
            "trace_agent_to_client"
        );

        let writer = self.trace_writer.as_mut().unwrap();

        match message {
            MessageCx::Request(_, request_cx) => {
                writer.request(protocol, from, to, request_cx.id(), &method, None, params);
            }
            MessageCx::Notification(_) => {
                writer.notification(protocol, from, to, &method, None, params);
            }
            MessageCx::Response(result, response_cx) => {
                writer.response(from, to, response_cx.id(), result.is_err(), params);
            }
        }
        Ok(())
    }

    /// Convert a ComponentIndex to a trace-friendly name.
    fn component_index_name(&self, index: ComponentIndex) -> String {
        match index {
            ComponentIndex::Client => "client".to_string(),
            ComponentIndex::Proxy(i) => format!("proxy:{}", i),
            ComponentIndex::Successor => "agent".to_string(),
        }
    }

    /// Recursively spawns components and builds the proxy chain.
    ///
    /// This function implements the recursive chain building pattern:
    /// 1. Pop the next component from the `providers` list
    /// 2. Create the component (either spawn subprocess or use mock)
    /// 3. Set up JSON-RPC connection and message handlers
    /// 4. Recursively call itself to spawn the next component
    /// 5. When no components remain, start the message routing loop via `serve()`
    ///
    /// Central message handling logic for the conductor.
    /// The conductor routes all [`ConductorMessage`] messages through to this function.
    /// Each message corresponds to a request or notification from one component to another.
    /// The conductor ferries messages from one place to another, sometimes making modifications along the way.
    /// Note that *responses to requests* are sent *directly* without going through this loop.
    ///
    /// The names we use are
    ///
    /// * The *client* is the originator of all ACP traffic, typically an editor or GUI.
    /// * Then there is a sequence of *components* consisting of:
    ///     * Zero or more *proxies*, which receive messages and forward them to the next component in the chain.
    ///     * And finally the *agent*, which is the final component in the chain and handles the actual work.
    ///
    /// For the most part, we just pass messages through the chain without modification, but there are a few exceptions:
    ///
    /// * We send `InitializeProxyRequest` to proxy components and `InitializeRequest` to the agent component.
    /// * We modify "session/new" requests that use `acp:...` as the URL for an MCP server to redirect
    ///   through a stdio server that runs on localhost and bridges messages.
    async fn handle_conductor_message(
        &mut self,
        client: JrConnectionCx<Link>,
        message: ConductorMessage,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(?message, "handle_conductor_message");

        match message {
            ConductorMessage::LeftToRight {
                target_component_index,
                message,
            } => {
                // Tracing happens inside forward_client_to_agent_message, after initialization,
                // so that component_name() has access to the populated proxies list.
                self.forward_client_to_agent_message(target_component_index, message, client)
                    .await
            }

            ConductorMessage::RightToLeft {
                source_component_index,
                message,
            } => {
                tracing::debug!(
                    ?source_component_index,
                    message_method = ?message.method(),
                    "Conductor: AgentToClient received"
                );
                if let Err(e) = self.trace_agent_to_client(source_component_index, &message) {
                    tracing::warn!("Failed to trace agent-to-client message: {e}");
                }
                self.send_message_to_predecessor_of(client, source_component_index, message)
            }

            // New MCP connection request. Send it back along the chain to get a connection id.
            // When the connection id arrives, send a message back into this conductor loop with
            // the connection id and the (as yet unspawned) actor.
            ConductorMessage::McpConnectionReceived {
                acp_url,
                connection,
                actor,
            } => {
                // MCP connection requests always come from the agent
                // (we must be in agent mode, in fact), so send the MCP request
                // to the final proxy.
                self.send_request_to_predecessor_of(
                    client,
                    self.proxies.len(),
                    McpConnectRequest {
                        acp_url,
                        meta: None,
                    },
                )
                .on_receiving_result({
                    let mut conductor_tx = self.conductor_tx.clone();
                    async move |result| {
                        match result {
                            Ok(response) => conductor_tx
                                .send(ConductorMessage::McpConnectionEstablished {
                                    response,
                                    actor,
                                    connection,
                                })
                                .await
                                .map_err(|_| sacp::Error::internal_error()),
                            Err(_) => {
                                // Error occurred, just drop the connection.
                                Ok(())
                            }
                        }
                    }
                })
            }

            // MCP connection successfully established. Spawn the actor
            // and insert the connection into our map fot future reference.
            ConductorMessage::McpConnectionEstablished {
                response: McpConnectResponse { connection_id, .. },
                actor,
                connection,
            } => {
                self.bridge_connections
                    .insert(connection_id.clone(), connection);
                client.spawn(actor.run(connection_id))
            }

            // Message meant for the MCP client received. Forward it to the appropriate actor's mailbox.
            ConductorMessage::McpClientToMcpServer {
                connection_id,
                message,
            } => {
                let wrapped = message.map(
                    |request, request_cx| {
                        (
                            McpOverAcpMessage {
                                connection_id: connection_id.clone(),
                                message: request,
                                meta: None,
                            },
                            request_cx,
                        )
                    },
                    |notification| McpOverAcpMessage {
                        connection_id: connection_id.clone(),
                        message: notification,
                        meta: None,
                    },
                );

                // We only get MCP-over-ACP requests when we are in bridging MCP for the final agent,
                // so send them to the final proxy.
                self.trace_agent_to_client(SourceComponentIndex::Successor, &wrapped)?;
                self.send_message_to_predecessor_of(
                    client,
                    SourceComponentIndex::Successor,
                    wrapped,
                )
            }

            // MCP client disconnected. Remove it from our map and send the
            // notification backwards along the chain.
            ConductorMessage::McpConnectionDisconnected { notification } => {
                // We only get MCP-over-ACP requests when we are in bridging MCP for the final agent.

                self.bridge_connections.remove(&notification.connection_id);
                self.send_notification_to_predecessor_of(client, self.proxies.len(), notification)
            }
        }
    }

    /// Send a message (request or notification) to the predecessor of the given component.
    ///
    /// This is a bit subtle because the relationship of the conductor
    /// is different depending on who will be receiving the message:
    /// * If the message is going to the conductor's client, then no changes
    ///   are needed, as the conductor is sending an agent-to-client message and
    ///   the conductor is acting as the agent.
    /// * If the message is going to a proxy component, then we have to wrap
    ///   it in a "from successor" wrapper, because the conductor is the
    ///   proxy's client.
    fn send_message_to_predecessor_of<Req: JrRequest, N: JrNotification>(
        &mut self,
        client: JrConnectionCx<Link>,
        source_component_index: SourceComponentIndex,
        message: MessageCx<Req, N>,
    ) -> Result<(), sacp::Error>
    where
        Req::Response: Send,
    {
        let source_component_index = match source_component_index {
            SourceComponentIndex::Successor => self.proxies.len(),
            SourceComponentIndex::Proxy(index) => index,
        };

        match message {
            MessageCx::Request(request, request_cx) => self
                .send_request_to_predecessor_of(client, source_component_index, request)
                .forward_to_request_cx(request_cx),
            MessageCx::Notification(notification) => self.send_notification_to_predecessor_of(
                client,
                source_component_index,
                notification,
            ),
            MessageCx::Response(result, response_cx) => response_cx.respond_with_result(result),
        }
    }

    fn send_request_to_predecessor_of<Req: JrRequest>(
        &mut self,
        client: JrConnectionCx<Link>,
        source_component_index: usize,
        request: Req,
    ) -> JrResponse<Req::Response> {
        if source_component_index == 0 {
            client.send_request_to(ClientPeer, request)
        } else {
            self.proxies[source_component_index - 1].send_request(SuccessorMessage {
                message: request,
                meta: None,
            })
        }
    }

    /// Send a notification to the predecessor of the given component.
    ///
    /// This is a bit subtle because the relationship of the conductor
    /// is different depending on who will be receiving the message:
    /// * If the notification is going to the conductor's client, then no changes
    ///   are needed, as the conductor is sending an agent-to-client message and
    ///   the conductor is acting as the agent.
    /// * If the notification is going to a proxy component, then we have to wrap
    ///   it in a "from successor" wrapper, because the conductor is the
    ///   proxy's client.
    fn send_notification_to_predecessor_of<N: JrNotification>(
        &mut self,
        client: JrConnectionCx<Link>,
        source_component_index: usize,
        notification: N,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(
            source_component_index,
            proxies_len = self.proxies.len(),
            "send_notification_to_predecessor_of"
        );
        if source_component_index == 0 {
            tracing::debug!("Sending notification directly to client");
            client.send_notification_to(ClientPeer, notification)
        } else {
            tracing::debug!(
                target_proxy = source_component_index - 1,
                "Sending notification wrapped as SuccessorMessage to proxy"
            );
            self.proxies[source_component_index - 1].send_notification(SuccessorMessage {
                message: notification,
                meta: None,
            })
        }
    }

    /// Send a message (request or notification) from 'left to right'.
    /// Left-to-right means from the client or an intermediate proxy to the component
    /// at `target_component_index` (could be a proxy or the agent).
    /// Makes changes to select messages along the way (e.g., `initialize` and `session/new`).
    async fn forward_client_to_agent_message(
        &mut self,
        target_component_index: usize,
        message: MessageCx,
        conductor_cx: JrConnectionCx<Link>,
    ) -> Result<(), sacp::Error> {
        tracing::trace!(
            target_component_index,
            ?message,
            "forward_client_to_agent_message"
        );

        // Ensure components are initialized before processing any message.
        let message = self
            .ensure_initialized(conductor_cx.clone(), message)
            .await?;

        // Trace after initialization so component_name() has access to the populated proxies list.
        if let Err(e) = self.trace_client_to_agent(target_component_index, &message) {
            tracing::warn!("Failed to trace client-to-agent message: {e}");
        }

        // In proxy mode, if the target is beyond our component chain,
        // forward to the conductor's own successor (via client connection)
        if target_component_index < self.proxies.len() {
            self.forward_message_to_proxy(target_component_index, message)
                .await
        } else {
            assert_eq!(target_component_index, self.proxies.len());

            debug!(
                target_component_index,
                proxies_count = self.proxies.len(),
                "Proxy mode: forwarding successor message to conductor's successor"
            );
            let successor = self.successor.clone();
            successor.send_message(message, conductor_cx, self).await
        }
    }

    /// Ensures components are initialized before processing messages.
    ///
    /// If components haven't been initialized yet, this expects the first message
    /// to be an `initialize` request and uses it to spawn the component chain.
    ///
    /// Returns:
    /// - `Ok(Some(message))` - Components are initialized, continue processing this message
    /// - `Ok(None)` - An error response was sent, caller should return early
    /// - `Err(_)` - A fatal error occurred
    async fn ensure_initialized(
        &mut self,
        client: JrConnectionCx<Link>,
        message: MessageCx,
    ) -> Result<MessageCx, Error> {
        // Already initialized - pass through
        let Some(instantiator) = self.instantiator.take() else {
            return Ok(message);
        };

        let message = self
            .link
            .initialize(message, client, instantiator, self)
            .await?;
        Ok(message)
    }

    /// Spawn proxy components and add them to the proxies list.
    fn spawn_proxies(
        &mut self,
        cx: JrConnectionCx<Link>,
        proxy_components: Vec<sacp::DynComponent<ProxyToConductor>>,
    ) -> Result<(), sacp::Error> {
        assert!(self.proxies.is_empty());

        let num_proxies = proxy_components.len();
        info!(proxy_count = num_proxies, "spawn_proxies");

        // Spawn each proxy component
        for (component_index, dyn_component) in proxy_components.into_iter().enumerate() {
            debug!(component_index, "spawning proxy");

            let proxy_cx = cx.spawn_connection(
                ConductorToProxy::builder()
                    .name(format!("conductor-to-component({})", component_index))
                    // Intercept messages sent by a proxy component to its successor.
                    .on_receive_message(
                        {
                            let mut conductor_tx = self.conductor_tx.clone();
                            type SuccessorMessageCx = MessageCx<SuccessorMessage, SuccessorMessage>;
                            async move |message_cx: SuccessorMessageCx, _cx| {
                                // Subtle point:
                                //
                                // `ConductorToProxy` has only a single peer, `AgentPeer`. This means that we see
                                // "successor messages" in their "desugared form". So when we intercept an *outgoing*
                                // message that matches `SuccessorMessage`, it could be one of three things
                                //
                                // - A request being sent by the proxy to its successor (hence going left->right)
                                // - A notification being sent by the proxy to its successor (hence going left->right)
                                // - A response to a request sent to the proxy *by* its successor. Here, the *request*
                                //   was going right->left, but the *response* (the message we are processing now)
                                //   is going left->right.
                                //
                                // So, in all cases, we forward as a left->right message.

                                conductor_tx
                                    .send(ConductorMessage::LeftToRight {
                                        target_component_index: component_index + 1,
                                        message: message_cx
                                            .map(|r, cx| (r.message, cx), |n| n.message),
                                    })
                                    .await
                                    .map_err(sacp::util::internal_error)
                            }
                        },
                        sacp::on_receive_message!(),
                    )
                    // Intercept agent-to-client messages from the proxy.
                    .on_receive_message(
                        {
                            let mut conductor_tx = self.conductor_tx.clone();
                            async move |message_cx: MessageCx, _cx| {
                                // As in the previous handler:
                                //
                                // Messages here are seen in their "desugared form", so we are seeing
                                // one of three things
                                //
                                // - A request being sent by the proxy to its predecessor (hence going right->left)
                                // - A notification being sent by the proxy to its predecessor (hence going right->left)
                                // - A response to a request sent to the proxy *by* its predecessor. Here, the *request*
                                //   was going left->right, but the *response* (the message we are processing now)
                                //   is going right->left.
                                //
                                // So, in all cases, we forward as a right->left message.

                                let message = ConductorMessage::RightToLeft {
                                    source_component_index: SourceComponentIndex::Proxy(
                                        component_index,
                                    ),
                                    message: message_cx,
                                };
                                conductor_tx
                                    .send(message)
                                    .await
                                    .map_err(sacp::util::internal_error)
                            }
                        },
                        sacp::on_receive_message!(),
                    )
                    .connect_to(dyn_component)?,
                |c| Box::pin(c.serve()),
            )?;
            self.proxies.push(proxy_cx);
        }

        info!(proxy_count = self.proxies.len(), "Proxies spawned");

        Ok(())
    }

    async fn forward_message_to_proxy(
        &mut self,
        target_component_index: usize,
        message: MessageCx,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(?message, "forward_message_to_proxy");

        MatchMessage::new(message)
            .if_request(async |_request: InitializeProxyRequest, request_cx| {
                request_cx.respond_with_error(
                    sacp::Error::invalid_request()
                        .data("initialize/proxy requests are only sent by the conductor"),
                )
            })
            .await
            .if_request(async |request: InitializeRequest, request_cx| {
                // The pattern for `Initialize` messages is a bit subtle.
                // Proxy receive incoming `Initialize` messages as if they
                // were a client. The conductor (us) intercepts these and
                // converts them to an `InitializeProxyRequest`.
                //
                // The proxy will then initialize itself and forward an `Initialize`
                // request to its successor.
                self.proxies[target_component_index]
                    .send_request(InitializeProxyRequest::from(request))
                    .on_receiving_result(async move |result| {
                        tracing::debug!(?result, "got initialize_proxy response from proxy");
                        request_cx.respond_with_result(result)
                    })
            })
            .await
            .otherwise(async |message| {
                // Otherwise, just send the message along "as is".
                self.proxies[target_component_index].send_proxied_message_to(AgentPeer, message)
            })
            .await
    }

    /// Invoked when sending a message from the conductor to the agent that it manages.
    /// This is called by `self.successor`'s [`ConductorSuccessor::send_message`]
    /// method when `Link = ConductorToClient` (i.e., the conductor is not itself
    /// running as a proxy).
    async fn forward_message_to_agent(
        &mut self,
        conductor_cx: JrConnectionCx<ConductorToClient>,
        message: MessageCx,
        agent_cx: JrConnectionCx<ConductorToAgent>,
    ) -> Result<(), Error> {
        MatchMessage::new(message)
            .if_request(async |_request: InitializeProxyRequest, request_cx| {
                request_cx.respond_with_error(
                    sacp::Error::invalid_request()
                        .data("initialize/proxy requests are only sent by the conductor"),
                )
            })
            .await
            .if_request(async |mut request: NewSessionRequest, request_cx| {
                // When forwarding "session/new" to the agent,
                // we adjust MCP servers to manage "acp:" URLs.
                for mcp_server in &mut request.mcp_servers {
                    self.bridge_listeners
                        .transform_mcp_server(
                            conductor_cx.clone(),
                            mcp_server,
                            &self.conductor_tx,
                            &self.mcp_bridge_mode,
                        )
                        .await?;
                }

                agent_cx
                    .send_request(request)
                    .forward_to_request_cx(request_cx)
            })
            .await
            .if_request(
                async |request: McpOverAcpMessage<UntypedMessage>, request_cx| {
                    let McpOverAcpMessage {
                        connection_id,
                        message: mcp_request,
                        ..
                    } = request;
                    self.bridge_connections
                        .get_mut(&connection_id)
                        .ok_or_else(|| {
                            sacp::util::internal_error(format!(
                                "unknown connection id: {}",
                                connection_id
                            ))
                        })?
                        .send(MessageCx::Request(mcp_request, request_cx))
                        .await
                },
            )
            .await
            .if_notification(async |notification: McpOverAcpMessage<UntypedMessage>| {
                let McpOverAcpMessage {
                    connection_id,
                    message: mcp_notification,
                    ..
                } = notification;
                self.bridge_connections
                    .get_mut(&connection_id)
                    .ok_or_else(|| {
                        sacp::util::internal_error(format!(
                            "unknown connection id: {}",
                            connection_id
                        ))
                    })?
                    .send(MessageCx::Notification(mcp_notification))
                    .await
            })
            .await
            .otherwise(async |message| {
                // Otherwise, just send the message along "as is".
                agent_cx.send_proxied_message_to(AgentPeer, message)
            })
            .await
    }
}

/// Identifies a component in the conductor's chain for tracing purposes.
///
/// Used to track message sources and destinations through the proxy chain.
#[derive(Debug, Clone, Copy)]
pub enum ComponentIndex {
    /// The client (editor) at the start of the chain.
    Client,

    /// A proxy component at the given index.
    Proxy(usize),

    /// The successor (agent in agent mode, outer conductor in proxy mode).
    Successor,
}

/// Identifies the source of an agent-to-client message.
///
/// This enum handles the fact that the conductor may receive messages from two different sources:
/// 1. From one of its managed components (identified by index)
/// 2. From the conductor's own successor in a larger proxy chain (when in proxy mode)
#[derive(Debug, Clone, Copy)]
pub enum SourceComponentIndex {
    /// Message from the conductor's agent or successor.
    Successor,

    /// Message from a specific component at the given index in the managed chain.
    Proxy(usize),
}

/// Trait for lazy proxy instantiation (proxy mode).
///
/// Used by conductors in proxy mode (`ConductorToConductor`) where all components
/// are proxies that forward to an outer conductor.
pub trait InstantiateProxies: Send {
    /// Instantiate proxy components based on the Initialize request.
    ///
    /// Returns proxy components typed as `DynComponent<ProxyToConductor>` since proxies
    /// communicate with the conductor.
    fn instantiate_proxies(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<(InitializeRequest, Vec<sacp::DynComponent<ProxyToConductor>>), sacp::Error>,
    >;
}

/// Simple implementation: provide all proxy components unconditionally.
///
/// Requires `T: Component<ProxyToConductor>`.
impl<T> InstantiateProxies for Vec<T>
where
    T: Component<ProxyToConductor> + 'static,
{
    fn instantiate_proxies(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<(InitializeRequest, Vec<sacp::DynComponent<ProxyToConductor>>), sacp::Error>,
    > {
        Box::pin(async move {
            let components: Vec<sacp::DynComponent<ProxyToConductor>> = (*self)
                .into_iter()
                .map(|c| sacp::DynComponent::new(c))
                .collect();
            Ok((req, components))
        })
    }
}

/// Dynamic implementation: closure receives the Initialize request and returns proxies.
impl<F, Fut> InstantiateProxies for F
where
    F: FnOnce(InitializeRequest) -> Fut + Send + 'static,
    Fut: std::future::Future<
            Output = Result<
                (InitializeRequest, Vec<sacp::DynComponent<ProxyToConductor>>),
                sacp::Error,
            >,
        > + Send
        + 'static,
{
    fn instantiate_proxies(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<(InitializeRequest, Vec<sacp::DynComponent<ProxyToConductor>>), sacp::Error>,
    > {
        Box::pin(async move { (*self)(req).await })
    }
}

/// Trait for lazy proxy and agent instantiation (agent mode).
///
/// Used by conductors in agent mode (`ConductorToClient`) where there are
/// zero or more proxies followed by an agent component.
pub trait InstantiateProxiesAndAgent: Send {
    /// Instantiate proxy and agent components based on the Initialize request.
    ///
    /// Returns the (possibly modified) request, a vector of proxy components
    /// (typed as `DynComponent<ProxyToConductor>`), and the agent component
    /// (typed as `DynComponent<AgentToClient>`).
    fn instantiate_proxies_and_agent(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<
            (
                InitializeRequest,
                Vec<sacp::DynComponent<ProxyToConductor>>,
                sacp::DynComponent<AgentToClient>,
            ),
            sacp::Error,
        >,
    >;
}

/// Wrapper to convert a single agent component (no proxies) into InstantiateProxiesAndAgent.
pub struct AgentOnly<A>(pub A);

impl<A: Component<AgentToClient> + 'static> InstantiateProxiesAndAgent for AgentOnly<A> {
    fn instantiate_proxies_and_agent(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<
            (
                InitializeRequest,
                Vec<sacp::DynComponent<ProxyToConductor>>,
                sacp::DynComponent<AgentToClient>,
            ),
            sacp::Error,
        >,
    > {
        Box::pin(async move { Ok((req, Vec::new(), sacp::DynComponent::new(self.0))) })
    }
}

/// Builder for creating proxies and agent components.
///
/// # Example
/// ```ignore
/// ProxiesAndAgent::new(ElizaAgent::new())
///     .proxy(LoggingProxy::new())
///     .proxy(AuthProxy::new())
/// ```
pub struct ProxiesAndAgent {
    proxies: Vec<sacp::DynComponent<ProxyToConductor>>,
    agent: sacp::DynComponent<AgentToClient>,
}

impl ProxiesAndAgent {
    /// Create a new builder with the given agent component.
    pub fn new(agent: impl Component<AgentToClient> + 'static) -> Self {
        Self {
            proxies: vec![],
            agent: sacp::DynComponent::new(agent),
        }
    }

    /// Add a single proxy component.
    pub fn proxy(mut self, proxy: impl Component<ProxyToConductor> + 'static) -> Self {
        self.proxies.push(sacp::DynComponent::new(proxy));
        self
    }

    /// Add multiple proxy components.
    pub fn proxies<P, I>(mut self, proxies: I) -> Self
    where
        P: Component<ProxyToConductor> + 'static,
        I: IntoIterator<Item = P>,
    {
        self.proxies
            .extend(proxies.into_iter().map(sacp::DynComponent::new));
        self
    }
}

impl InstantiateProxiesAndAgent for ProxiesAndAgent {
    fn instantiate_proxies_and_agent(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<
            (
                InitializeRequest,
                Vec<sacp::DynComponent<ProxyToConductor>>,
                sacp::DynComponent<AgentToClient>,
            ),
            sacp::Error,
        >,
    > {
        Box::pin(async move { Ok((req, self.proxies, self.agent)) })
    }
}

/// Dynamic implementation: closure receives the Initialize request and returns proxies + agent.
impl<F, Fut> InstantiateProxiesAndAgent for F
where
    F: FnOnce(InitializeRequest) -> Fut + Send + 'static,
    Fut: std::future::Future<
            Output = Result<
                (
                    InitializeRequest,
                    Vec<sacp::DynComponent<ProxyToConductor>>,
                    sacp::DynComponent<AgentToClient>,
                ),
                sacp::Error,
            >,
        > + Send
        + 'static,
{
    fn instantiate_proxies_and_agent(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<
            (
                InitializeRequest,
                Vec<sacp::DynComponent<ProxyToConductor>>,
                sacp::DynComponent<AgentToClient>,
            ),
            sacp::Error,
        >,
    > {
        Box::pin(async move { (*self)(req).await })
    }
}

/// Messages sent to the conductor's main event loop for routing.
///
/// These messages enable the conductor to route communication between:
/// - The editor and the first component
/// - Components and their successors in the chain
/// - Components and their clients (editor or predecessor)
///
/// All spawned tasks send messages via this enum through a shared channel,
/// allowing centralized routing logic in the `serve()` loop.
#[derive(Debug)]
pub enum ConductorMessage {
    /// If this message is a request or notification, then it is going "left-to-right"
    /// (e.g., a component making a request of its successor).
    ///
    /// If this message is a response, then it is going right-to-left
    /// (i.e., the successor answering a request made by its predecessor).
    LeftToRight {
        target_component_index: usize,
        message: MessageCx,
    },

    /// If this message is a request or notification, then it is going "right-to-left"
    /// (e.g., a component making a request of its predecessor).
    ///
    /// If this message is a response, then it is going "left-to-right"
    /// (i.e., the predecessor answering a request made by its successor).
    RightToLeft {
        source_component_index: SourceComponentIndex,
        message: MessageCx,
    },

    /// A pending MCP bridge connection request request.
    /// The request must be sent back over ACP to receive the connection-id.
    /// Once the connection-id is received, the actor must be spawned.
    McpConnectionReceived {
        /// The acp:$UUID URL identifying this bridge
        acp_url: String,

        /// The actor that should be spawned once the connection-id is available.
        actor: McpBridgeConnectionActor,

        /// The connection to the bridge
        connection: McpBridgeConnection,
    },

    /// A pending MCP bridge connection request request.
    /// The request must be sent back over ACP to receive the connection-id.
    /// Once the connection-id is received, the actor must be spawned.
    McpConnectionEstablished {
        response: McpConnectResponse,

        /// The actor that should be spawned once the connection-id is available.
        actor: McpBridgeConnectionActor,

        /// The connection to the bridge
        connection: McpBridgeConnection,
    },

    /// MCP message (request or notification) received from a bridge that needs to be routed to the final proxy.
    ///
    /// Sent when the bridge receives an MCP tool call from the agent and forwards it
    /// to the conductor via TCP. The conductor routes this to the appropriate proxy component.
    McpClientToMcpServer {
        connection_id: String,
        message: MessageCx,
    },

    /// Message sent when MCP client disconnects
    McpConnectionDisconnected {
        notification: McpDisconnectNotification,
    },
}

/// Trait implemented for the two links the conductor can use:
///
/// * ConductorToClient -- conductor is acting as an agent, so when its last proxy sends to its successor, the conductor sends that message to its agent component
/// * ConductorToConductor -- conductor is acting as a proxy, so when its last proxy sends to its successor, the (inner) conductor sends that message to its successor, via the outer conductor
pub trait ConductorLink: JrLink + HasPeer<ClientPeer> {
    type Speaks: JrLink<ConnectsTo = Self::ConnectsTo>;

    /// The type used to instantiate components for this link type.
    type Instantiator: Send;

    /// Handle initialization: parse the init request, instantiate components, and spawn them.
    ///
    /// Takes ownership of the instantiator and returns the (possibly modified) init request
    /// wrapped in a MessageCx for forwarding.
    fn initialize(
        self,
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        instantiator: Self::Instantiator,
        responder: &mut ConductorResponder<Self>,
    ) -> impl Future<Output = Result<MessageCx, sacp::Error>> + Send;

    /// Handle an incoming message from the client or conductor, depending on `Self`
    fn handle_message(
        self,
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
    ) -> impl Future<Output = Result<Handled<MessageCx>, sacp::Error>> + Send;
}

impl ConductorLink for ConductorToClient {
    /// In this mode, the conductor acts as an agent talking to a client.
    type Speaks = AgentToClient;

    type Instantiator = Box<dyn InstantiateProxiesAndAgent>;

    async fn initialize(
        self,
        message: MessageCx,
        client: JrConnectionCx<Self>,
        instantiator: Self::Instantiator,
        responder: &mut ConductorResponder<Self>,
    ) -> Result<MessageCx, sacp::Error> {
        let invalid_request = || Error::invalid_request().data("expected `initialize` request");

        // Not yet initialized - expect an initialize request.
        // Error if we get anything else.
        let MessageCx::Request(request, request_cx) = message else {
            message.respond_with_error(invalid_request(), client.clone())?;
            return Err(invalid_request());
        };
        if !InitializeRequest::matches_method(request.method()) {
            request_cx.respond_with_error(invalid_request())?;
            return Err(invalid_request());
        }

        let init_request =
            match InitializeRequest::parse_message(request.method(), request.params()) {
                Ok(r) => r,
                Err(error) => {
                    request_cx.respond_with_error(error)?;
                    return Err(invalid_request());
                }
            };

        // Instantiate proxies and agent
        let (modified_req, proxy_components, agent_component) = instantiator
            .instantiate_proxies_and_agent(init_request)
            .await?;

        // Spawn the agent component
        debug!(?agent_component, "spawning agent");
        let agent_cx = client.spawn_connection(
            ConductorToAgent::builder()
                .name("conductor-to-agent")
                // Intercept agent-to-client messages from the agent.
                .on_receive_message(
                    {
                        let mut conductor_tx = responder.conductor_tx.clone();
                        async move |message_cx: MessageCx, _cx| {
                            conductor_tx
                                .send(ConductorMessage::RightToLeft {
                                    source_component_index: SourceComponentIndex::Successor,
                                    message: message_cx,
                                })
                                .await
                                .map_err(sacp::util::internal_error)
                        }
                    },
                    sacp::on_receive_message!(),
                )
                .connect_to(agent_component)?,
            |c| Box::pin(c.serve()),
        )?;
        responder.successor = Arc::new(agent_cx);

        // Spawn the proxy components
        responder.spawn_proxies(client.clone(), proxy_components)?;

        Ok(MessageCx::Request(
            modified_req.to_untyped_message()?,
            request_cx,
        ))
    }

    async fn handle_message(
        self,
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
    ) -> Result<Handled<MessageCx>, sacp::Error> {
        tracing::debug!(
            method = ?message.method(),
            "ConductorToClient::handle_message"
        );
        MatchMessageFrom::new(message, &cx)
            // Any incoming messages from the client are client-to-agent messages targeting the first component.
            .if_message_from(ClientPeer, async move |message: MessageCx| {
                tracing::debug!(
                    method = ?message.method(),
                    "ConductorToClient::handle_message - matched Client"
                );
                Conductor::<Self>::incoming_message_from_client(conductor_tx, message).await
            })
            .await
            .done()
    }
}

impl ConductorLink for ConductorToConductor {
    /// In this mode, the conductor acts as a proxy talking to an (outer) conductor..
    type Speaks = ProxyToConductor;

    type Instantiator = Box<dyn InstantiateProxies>;

    async fn initialize(
        self,
        message: MessageCx,
        client_cx: JrConnectionCx<Self>,
        instantiator: Self::Instantiator,
        responder: &mut ConductorResponder<Self>,
    ) -> Result<MessageCx, sacp::Error> {
        let invalid_request = || Error::invalid_request().data("expected `initialize` request");

        // Not yet initialized - expect an InitializeProxy request.
        // Error if we get anything else.
        let MessageCx::Request(request, request_cx) = message else {
            message.respond_with_error(invalid_request(), client_cx.clone())?;
            return Err(invalid_request());
        };
        if !InitializeProxyRequest::matches_method(request.method()) {
            request_cx.respond_with_error(invalid_request())?;
            return Err(invalid_request());
        }

        let InitializeProxyRequest { initialize } =
            match InitializeProxyRequest::parse_message(request.method(), request.params()) {
                Ok(r) => r,
                Err(error) => {
                    request_cx.respond_with_error(error)?;
                    return Err(invalid_request());
                }
            };

        tracing::debug!("ensure_initialized: InitializeProxyRequest (proxy mode)");

        // Instantiate proxies (no agent in proxy mode)
        let (modified_req, proxy_components) = instantiator.instantiate_proxies(initialize).await?;

        // In proxy mode, our successor is the outer conductor (via our client connection)
        responder.successor = Arc::new(());

        // Spawn the proxy components
        responder.spawn_proxies(client_cx.clone(), proxy_components)?;

        Ok(MessageCx::Request(
            modified_req.to_untyped_message()?,
            request_cx,
        ))
    }

    async fn handle_message(
        self,
        message: MessageCx,
        cx: JrConnectionCx<Self>,
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
    ) -> Result<Handled<MessageCx>, sacp::Error> {
        tracing::debug!(
            method = ?message.method(),
            ?message,
            "ConductorToConductor::handle_message"
        );
        MatchMessageFrom::new(message, &cx)
            .if_message_from(AgentPeer, {
                // Messages from our successor arrive already unwrapped
                // (RemoteRoleStyle::Successor strips the SuccessorMessage envelope).
                async |message: MessageCx| {
                    tracing::debug!(
                        method = ?message.method(),
                        "ConductorToConductor::handle_message - matched Agent"
                    );
                    let mut conductor_tx = conductor_tx.clone();
                    Conductor::<Self>::incoming_message_from_agent(&mut conductor_tx, message).await
                }
            })
            .await
            // Any incoming messages from the client are client-to-agent messages targeting the first component.
            .if_message_from(ClientPeer, async |message: MessageCx| {
                tracing::debug!(
                    method = ?message.method(),
                    "ConductorToConductor::handle_message - matched Client"
                );
                let mut conductor_tx = conductor_tx.clone();
                Conductor::<Self>::incoming_message_from_client(&mut conductor_tx, message).await
            })
            .await
            .done()
    }
}

pub trait ConductorSuccessor<Link: ConductorLink>: Send + Sync + 'static {
    fn send_message<'a>(
        &self,
        message: MessageCx,
        conductor_cx: JrConnectionCx<Link>,
        responder: &'a mut ConductorResponder<Link>,
    ) -> BoxFuture<'a, Result<(), sacp::Error>>;
}

impl<Link: ConductorLink> ConductorSuccessor<Link> for sacp::Error {
    fn send_message<'a>(
        &self,
        #[expect(unused_variables)] message: MessageCx,
        #[expect(unused_variables)] conductor_cx: JrConnectionCx<Link>,
        #[expect(unused_variables)] responder: &'a mut ConductorResponder<Link>,
    ) -> BoxFuture<'a, Result<(), sacp::Error>> {
        let error = self.clone();
        Box::pin(std::future::ready(Err(error)))
    }
}

impl ConductorSuccessor<ConductorToConductor> for () {
    fn send_message<'a>(
        &self,
        message: MessageCx,
        conductor_cx: JrConnectionCx<ConductorToConductor>,
        _responder: &'a mut ConductorResponder<ConductorToConductor>,
    ) -> BoxFuture<'a, Result<(), sacp::Error>> {
        Box::pin(async move {
            debug!("Proxy mode: forwarding successor message to conductor's successor");
            conductor_cx.send_proxied_message_to(AgentPeer, message)
        })
    }
}

impl ConductorSuccessor<ConductorToClient> for JrConnectionCx<ConductorToAgent> {
    fn send_message<'a>(
        &self,
        message: MessageCx,
        conductor_cx: JrConnectionCx<ConductorToClient>,
        responder: &'a mut ConductorResponder<ConductorToClient>,
    ) -> BoxFuture<'a, Result<(), sacp::Error>> {
        let agent_cx = self.clone();
        Box::pin(async move {
            debug!("Proxy mode: forwarding successor message to conductor's successor");
            responder
                .forward_message_to_agent(conductor_cx, message, agent_cx)
                .await
        })
    }
}
