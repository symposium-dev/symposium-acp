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
//! Components discover their role via the initialization request type they receive:
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

use std::collections::HashMap;

use futures::{
    SinkExt, StreamExt,
    channel::mpsc::{self},
};
use sacp::{ChainResponder, JrResponder, NullResponder, role::{ConductorToAgent, ConductorToClient, ConductorToProxy}};
use sacp::schema::{
    McpConnectRequest, McpConnectResponse, McpDisconnectNotification, McpOverAcpMessage,
    SuccessorMessage,
};
use sacp::{Agent, Client, Component, Error, JrMessage};
use sacp::{
    HasDefaultEndpoint, JrConnectionBuilder, JrConnectionCx, JrNotification, JrRequest,
    JrRequestCx, JrResponse, JrRole, MessageCx, UntypedMessage,
};
use sacp::{
    JrMessageHandler, JrResponsePayload,
    schema::{
        InitializeProxyRequest, InitializeRequest, InitializeResponse, NewSessionRequest,
        NewSessionResponse,
    },
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
pub struct Conductor {
    name: String,
    component_list: Box<dyn ComponentList>,
    mcp_bridge_mode: crate::McpBridgeMode,
    trace_writer: Option<crate::trace::TraceWriter>,
}

impl Conductor {
    pub fn new(
        name: String,
        component_list: impl ComponentList + 'static,
        mcp_bridge_mode: crate::McpBridgeMode,
    ) -> Self {
        Conductor {
            name,
            component_list: Box::new(component_list),
            mcp_bridge_mode,
            trace_writer: None,
        }
    }

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
    ) -> JrConnectionBuilder<ConductorMessageHandler, ChainResponder<NullResponder, ConductorResponder>>
    {
        let (conductor_tx, conductor_rx) = mpsc::channel(128 /* chosen arbitrarily */);

        let responder = ConductorResponder {
            conductor_rx,
            conductor_tx: conductor_tx.clone(),
            component_list: Some(self.component_list),
            bridge_listeners: Default::default(),
            bridge_connections: Default::default(),
            mcp_bridge_mode: self.mcp_bridge_mode,
            proxies: Default::default(),
            agent: None,
            trace_writer: self.trace_writer,
            pending_requests: Default::default(),
        };

        JrConnectionBuilder::new_with(ConductorMessageHandler {
            conductor_tx,
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
    pub async fn run(self, transport: impl Component + 'static) -> Result<(), sacp::Error> {
        self.into_connection_builder()
            .connect_to(transport)?
            .serve()
            .await
    }
}

impl sacp::Component for Conductor {
    async fn serve(self, client: impl sacp::Component) -> Result<(), sacp::Error> {
        self.run(client).await
    }
}

pub struct ConductorMessageHandler {
    conductor_tx: mpsc::Sender<ConductorMessage>,
}


impl JrMessageHandler for ConductorMessageHandler {
    type Role = ConductorToClient;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        cx: sacp::JrConnectionCx<ConductorToClient>,
    ) -> Result<sacp::Handled<MessageCx>, sacp::Error> {
        ConductorToClient::builder()
            .on_receive_message_from(Agent, {
                let mut conductor_tx = self.conductor_tx.clone();
                // Messages from our successor arrive already unwrapped
                // (RemoteRoleStyle::Successor strips the SuccessorMessage envelope).
                async move |message: MessageCx, _cx| {
                    conductor_tx
                        .send(ConductorMessage::AgentToClient {
                            source_component_index: SourceComponentIndex::ConductorSuccessor,
                            message,
                        })
                        .await
                        .map_err(sacp::util::internal_error)
                }
            })
            // Any incoming messages from the client are client-to-agent messages targeting the first component.
            .on_receive_message_from(Client, {
                let mut conductor_tx = self.conductor_tx.clone();
                async move |message: MessageCx, _cx| {
                    conductor_tx
                        .send(ConductorMessage::ClientToAgent {
                            target_component_index: 0,
                            message,
                        })
                        .await
                        .map_err(sacp::util::internal_error)
                }
            })
            .apply(message, cx)
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
pub struct ConductorResponder {
    conductor_rx: mpsc::Receiver<ConductorMessage>,

    conductor_tx: mpsc::Sender<ConductorMessage>,

    /// Manages the TCP listeners for MCP connections that will be proxied over ACP.
    bridge_listeners: McpBridgeListeners,

    /// Manages active connections to MCP clients.
    bridge_connections: HashMap<String, McpBridgeConnection>,

    /// The component list for lazy initialization.
    /// Set to None after components are instantiated.
    component_list: Option<Box<dyn ComponentList>>,

    /// The chain of proxies before the agent (if any).
    ///
    /// Populated lazily when the first Initialize request is received.
    proxies: Vec<JrConnectionCx<ConductorToProxy>>,

    /// If the conductor is operating in agent mode, this will be the agent.
    /// If the conductor is operating in proxy mode, this will be None.
    ///
    /// Populated lazily when the first Initialize request is received.
    agent: Option<JrConnectionCx<ConductorToAgent>>,

    /// Mode for the MCP bridge (determines how to spawn bridge processes).
    mcp_bridge_mode: crate::McpBridgeMode,

    /// Optional trace writer for sequence diagram visualization.
    trace_writer: Option<crate::trace::TraceWriter>,

    /// Tracks pending requests for response tracing: id -> (from, to)
    pending_requests: HashMap<String, (String, String)>,
}

impl JrResponder<ConductorToClient> for ConductorResponder {
    async fn run(mut self, cx: JrConnectionCx<ConductorToClient>) -> Result<(), sacp::Error> {
        // Components are now spawned lazily in forward_initialize_request
        // when the first Initialize request is received.

        let mut conductor_tx = self.conductor_tx.clone();

        // This is the "central actor" of the conductor. Most other things forward messages
        // via `conductor_tx` into this loop. This lets us serialize the conductor's activity.
        while let Some(message) = self.conductor_rx.next().await {
            self
                .handle_conductor_message(&cx, message, &mut conductor_tx)
                .await?;
        }
        Ok(())
    }
}

impl ConductorResponder {
    /// Convert a component index to a trace-friendly name.
    fn component_name(&self, index: usize) -> String {
        if self.is_agent_component(index) {
            "agent".to_string()
        } else {
            format!("proxy:{}", index)
        }
    }

    /// Convert a source component index to a trace-friendly name.
    fn source_component_name(&self, index: SourceComponentIndex) -> String {
        match index {
            SourceComponentIndex::ConductorSuccessor => "agent".to_string(), // In proxy mode, successor is effectively the agent
            SourceComponentIndex::Component(i) => self.component_name(i),
        }
    }

    /// Extract the protocol and idealized method/params from a message.
    ///
    /// For MCP-over-ACP messages, this extracts the inner MCP message.
    /// For regular ACP messages, returns the message as-is.
    fn extract_trace_info<R: sacp::JrRequest, N: sacp::JrNotification>(
        message: &sacp::MessageCx<R, N>,
    ) -> Result<(crate::trace::Protocol, String, serde_json::Value), sacp::Error> {
        match message {
            sacp::MessageCx::Request(request, _) => {
                let untyped = request.to_untyped_message()?;

                // Try to parse as MCP-over-ACP request
                if let Some(Ok(mcp_req)) = <McpOverAcpMessage<UntypedMessage>>::parse_message(
                    &untyped.method,
                    &untyped.params,
                ) {
                    return Ok((
                        crate::trace::Protocol::Mcp,
                        mcp_req.message.method,
                        mcp_req.message.params,
                    ));
                }

                // Regular ACP request
                Ok((crate::trace::Protocol::Acp, untyped.method, untyped.params))
            }
            sacp::MessageCx::Notification(notification) => {
                let untyped = notification.to_untyped_message()?;

                // Try to parse as MCP-over-ACP notification
                if let Some(Ok(mcp_notif)) = <McpOverAcpMessage<UntypedMessage>>::parse_message(
                    &untyped.method,
                    &untyped.params,
                ) {
                    return Ok((
                        crate::trace::Protocol::Mcp,
                        mcp_notif.message.method,
                        mcp_notif.message.params,
                    ));
                }

                // Regular ACP notification
                Ok((crate::trace::Protocol::Acp, untyped.method, untyped.params))
            }
        }
    }

    /// Trace a client-to-agent message (request or notification).
    fn trace_client_to_agent<R: sacp::JrRequest, N: sacp::JrNotification>(
        &mut self,
        target_index: usize,
        message: &sacp::MessageCx<R, N>,
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

        let writer = self.trace_writer.as_mut().unwrap();
        match message.id() {
            Some(id) => {
                // Track pending request for response correlation
                let id_key = id.to_string();
                self.pending_requests
                    .insert(id_key, (from.clone(), to.clone()));
                writer.request(protocol, from, to, id, &method, None, params);
            }
            None => {
                writer.notification(protocol, from, to, &method, None, params);
            }
        }
        Ok(())
    }

    /// Trace an agent-to-client message (request or notification).
    fn trace_agent_to_client<R: sacp::JrRequest, N: sacp::JrNotification>(
        &mut self,
        source_index: SourceComponentIndex,
        message: &sacp::MessageCx<R, N>,
    ) -> Result<(), sacp::Error> {
        if self.trace_writer.is_none() {
            return Ok(());
        }

        let from = self.source_component_name(source_index);
        let to = match source_index {
            SourceComponentIndex::ConductorSuccessor => {
                if self.proxies.is_empty() {
                    "client".to_string()
                } else {
                    self.component_name(self.proxies.len() - 1)
                }
            }
            SourceComponentIndex::Component(0) => "client".to_string(),
            SourceComponentIndex::Component(i) => self.component_name(i - 1),
        };

        let (protocol, method, params) = Self::extract_trace_info(message)?;

        let writer = self.trace_writer.as_mut().unwrap();
        match message.id() {
            Some(id) => {
                // Track pending request for response correlation
                let id_key = id.to_string();
                self.pending_requests
                    .insert(id_key, (from.clone(), to.clone()));
                writer.request(protocol, from, to, id, &method, None, params);
            }
            None => {
                writer.notification(protocol, from, to, &method, None, params);
            }
        }
        Ok(())
    }

    /// Trace a response to a previous request.
    fn trace_response(
        &mut self,
        request_cx: &sacp::JrRequestCx<serde_json::Value>,
        result: &Result<serde_json::Value, sacp::Error>,
    ) {
        let Some(writer) = &mut self.trace_writer else {
            return;
        };

        let id = request_cx.id();
        let id_key = id.to_string();

        // Look up the original request's from/to (response goes in reverse)
        if let Some((original_from, original_to)) = self.pending_requests.remove(&id_key) {
            let (is_error, payload) = match result {
                Ok(v) => (false, v.clone()),
                Err(e) => (true, serde_json::json!({ "error": e.to_string() })),
            };
            // Response goes from original_to back to original_from
            writer.response(&original_to, &original_from, id, is_error, payload);
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
        client: &JrConnectionCx<ConductorToClient>,
        message: ConductorMessage,
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(?message, "handle_conductor_message");

        match message {
            ConductorMessage::ClientToAgent {
                target_component_index,
                message,
            } => {
                if let Err(e) = self.trace_client_to_agent(target_component_index, &message) {
                    tracing::warn!("Failed to trace client-to-agent message: {e}");
                }
                self.forward_client_to_agent_message(
                    conductor_tx,
                    target_component_index,
                    message,
                    client,
                )
                .await
            }

            ConductorMessage::AgentToClient {
                source_component_index,
                message,
            } => {
                tracing::debug!(
                    ?source_component_index,
                    message_method = ?message.message().method(),
                    "Conductor: AgentToClient received"
                );
                if let Err(e) = self.trace_agent_to_client(source_component_index, &message) {
                    tracing::warn!("Failed to trace agent-to-client message: {e}");
                }
                self.send_message_to_predecessor_of(
                    conductor_tx,
                    client,
                    source_component_index,
                    message,
                )
            }

            // New MCP connection request. Send it back along the chain to get a connection id.
            // When the connection id arrives, send a message back into this conductor loop with
            // the connection id and the (as yet unspawned) actor.
            ConductorMessage::McpConnectionReceived {
                acp_url,
                connection,
                actor,
            } => {
                // We only get MCP-over-ACP requests when we are in bridging MCP for the final agent.
                assert!(self.agent.is_some());

                // Send the MCP request to the predecessor of the final agent
                self.send_request_to_predecessor_of(
                    client,
                    self.proxies.len(),
                    McpConnectRequest {
                        acp_url,
                        meta: None,
                    },
                )
                .await_when_result_received({
                    let mut conductor_tx = conductor_tx.clone();
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
                // We only get MCP-over-ACP requests when we are in bridging MCP for the final agent.
                assert!(self.agent.is_some());

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

                let source_component = SourceComponentIndex::Component(self.proxies.len());
                self.trace_agent_to_client(source_component, &wrapped)?;
                self.send_message_to_predecessor_of(conductor_tx, client, source_component, wrapped)
            }

            // MCP client disconnected. Remove it from our map and send the
            // notification backwards along the chain.
            ConductorMessage::McpConnectionDisconnected { notification } => {
                // We only get MCP-over-ACP requests when we are in bridging MCP for the final agent.
                assert!(self.agent.is_some());

                self.bridge_connections.remove(&notification.connection_id);
                self.send_notification_to_predecessor_of(client, self.proxies.len(), notification)
            }

            // Forward a response back to the original request context.
            // This ensures responses are processed in order with notifications by
            // going through the central conductor queue.
            ConductorMessage::ForwardResponse { request_cx, result } => {
                self.trace_response(&request_cx, &result);
                request_cx.respond_with_result(result)
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
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        client: &JrConnectionCx<ConductorToClient>,
        source_component_index: SourceComponentIndex,
        message: MessageCx<Req, N>,
    ) -> Result<(), sacp::Error>
    where
        Req::Response: Send,
    {
        let source_component_index = match source_component_index {
            SourceComponentIndex::ConductorSuccessor => {
                // If message is coming from the conductor's successor,
                // check whether we were initialized as a proxy (no agent means we're a proxy).
                if self.agent.is_some() {
                    return Err(sacp::Error::invalid_request().with_data(
                        "cannot accept successor message when not initialized as a proxy",
                    ));
                }

                self.proxies.len()
            }

            SourceComponentIndex::Component(index) => index,
        };

        match message {
            MessageCx::Request(request, request_cx) => self
                .send_request_to_predecessor_of(client, source_component_index, request)
                .forward_response_via(conductor_tx, request_cx),
            MessageCx::Notification(notification) => self.send_notification_to_predecessor_of(
                client,
                source_component_index,
                notification,
            ),
        }
    }

    fn send_request_to_predecessor_of<Req: JrRequest>(
        &mut self,
        client: &JrConnectionCx<ConductorToClient>,
        source_component_index: usize,
        request: Req,
    ) -> JrResponse<Req::Response> {
        if source_component_index == 0 {
            client.send_request(request)
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
        client: &JrConnectionCx<ConductorToClient>,
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
            client.send_notification(notification)
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
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
        target_component_index: usize,
        message: MessageCx,
        connection_cx: &JrConnectionCx<ConductorToClient>,
    ) -> Result<(), sacp::Error> {
        tracing::trace!(
            target_component_index,
            ?message,
            "forward_client_to_agent_message"
        );

        // Ensure components are initialized before processing any message.
        let Some(message) = self
            .ensure_initialized(conductor_tx, connection_cx, message)
            .await?
        else {
            return Ok(());
        };

        // In proxy mode, if the target is beyond our component chain,
        // forward to the conductor's own successor (via client connection)
        if self.agent.is_none() && target_component_index == self.proxies.len() {
            debug!(
                target_component_index,
                proxies_count = self.proxies.len(),
                "Proxy mode: forwarding successor message to conductor's successor"
            );
            // Wrap the message as a successor message before sending
            let to_successor_message = message.map(
                |request, request_cx| {
                    (
                        SuccessorMessage {
                            message: request,
                            meta: None,
                        },
                        request_cx,
                    )
                },
                |notification| SuccessorMessage {
                    message: notification,
                    meta: None,
                },
            );
            return connection_cx.send_proxied_message_to(Client, to_successor_message);
        }

        tracing::debug!(?message, "forward_client_to_agent_message");

        MatchMessageFrom::new(message, connection_cx)
            .if_request(async |request: InitializeProxyRequest, request_cx| {
                // Proxy forwarding InitializeProxyRequest to its successor
                tracing::debug!("forward_client_to_agent_message: InitializeProxyRequest");
                // Wrap the request_cx to convert InitializeResponse back to InitializeProxyResponse
                self.forward_initialize_request(
                    target_component_index,
                    conductor_tx,
                    connection_cx,
                    request.initialize,
                    request_cx,
                )
                .await
            })
            .await
            .if_request(async |request: InitializeRequest, request_cx| {
                // Direct InitializeRequest (shouldn't happen after initialization, but handle it)
                tracing::debug!("forward_client_to_agent_message: InitializeRequest");
                self.forward_initialize_request(
                    target_component_index,
                    conductor_tx,
                    connection_cx,
                    request,
                    request_cx,
                )
                .await
            })
            .await
            .if_request(async |request: NewSessionRequest, request_cx| {
                // When forwarding "session/new", we adjust MCP servers to manage "acp:" URLs.
                self.forward_session_new_request(
                    target_component_index,
                    request,
                    &conductor_tx,
                    request_cx,
                    connection_cx,
                )
                .await
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
                if target_component_index == self.proxies.len() {
                    self.agent
                        .as_ref()
                        .expect("targeting agent")
                        .send_proxied_message_via(conductor_tx, message)
                } else {
                    self.proxies[target_component_index]
                        .send_proxied_message_via(conductor_tx, message)
                }
            })
            .await
    }

    /// Checks if the given component index is the agent.
    ///
    /// Note that, in proxy mode, there is no agent.
    /// Also, if there are no components, there is no agent.
    fn is_agent_component(&self, component_index: usize) -> bool {
        self.agent.is_some() && component_index == self.proxies.len()
    }

    /// Forward an initialize request to the appropriate component.
    ///
    /// Proxies receive `InitializeProxyRequest`, agents receive `InitializeRequest`.
    async fn forward_initialize_request(
        &mut self,
        target_component_index: usize,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        connection_cx: &JrConnectionCx<ConductorToClient>,
        initialize_req: InitializeRequest,
        request_cx: JrRequestCx<InitializeResponse>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(
            target_component_index,
            ?initialize_req,
            "forward_initialize_request"
        );

        let is_agent = self.is_agent_component(target_component_index);
        tracing::debug!(?is_agent, "forward_initialize_request");

        let conductor_tx = conductor_tx.clone();

        if is_agent {
            // Agent component - send InitializeRequest
            self.agent
                .as_ref()
                .expect("we have an agent component")
                .send_request(initialize_req)
                .await_when_result_received(async move |response| {
                    tracing::debug!(?response, "got initialize response from agent");
                    request_cx
                        .respond_with_result_via(conductor_tx, response)
                        .await
                })
        } else if target_component_index == self.proxies.len() {
            // Zero components case - we're in proxy mode with no local components.
            // Forward to our successor (the conductor's own successor).
            assert!(self.proxies.is_empty());

            if self.agent.is_some() {
                return Err(sacp::util::internal_error(
                    "conductor has no agent component",
                ));
            }

            // Forward initialize request to our successor
            connection_cx
                .send_request_to(Agent, initialize_req)
                .await_when_result_received(async move |result| {
                    tracing::trace!(
                        ?result,
                        "received response to initialize_proxy from empty conductor"
                    );
                    request_cx
                        .respond_with_result_via(conductor_tx, result)
                        .await
                })
        } else {
            // We convert an `InitializeRequest` to an `InitializeProxyRequest`
            // on the way to one of the proxies we are managing.
            assert!(target_component_index < self.proxies.len());

            let proxy_req = InitializeProxyRequest::from(initialize_req);
            self.proxies[target_component_index]
                .send_request(proxy_req)
                .await_when_result_received(async move |result| {
                    tracing::debug!(?result, "got initialize_proxy response from proxy");
                    // Convert InitializeProxyResponse back to InitializeResponse
                    request_cx
                        .respond_with_result_via(conductor_tx, result)
                        .await
                })
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
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
        client: &JrConnectionCx<ConductorToClient>,
        message: MessageCx,
    ) -> Result<Option<MessageCx>, Error> {
        // Already initialized - pass through
        if self.component_list.is_none() {
            return Ok(Some(message));
        }

        // Not yet initialized - expect an initialize or initialize_proxy request
        match message {
            MessageCx::Request(request, request_cx) => {
                // Try parsing as InitializeProxyRequest first (proxy mode)
                if let Some(result) =
                    InitializeProxyRequest::parse_message(request.method(), request.params())
                {
                    match result {
                        Ok(proxy_init_request) => {
                            tracing::debug!(
                                "ensure_initialized: InitializeProxyRequest (proxy mode)"
                            );
                            let (modified_request, modified_request_cx) = self
                                .lazy_initialize_components(
                                    conductor_tx,
                                    client,
                                    proxy_init_request.initialize,
                                    true, // proxy_mode
                                    request_cx.cast(),
                                )
                                .await?;
                            let untyped = modified_request.to_untyped_message()?;
                            Ok(Some(MessageCx::Request(
                                untyped,
                                modified_request_cx.erase_to_json(),
                            )))
                        }
                        Err(error) => {
                            request_cx.respond_with_error(error)?;
                            Ok(None)
                        }
                    }
                }
                // Try parsing as InitializeRequest (agent mode)
                else if let Some(result) =
                    InitializeRequest::parse_message(request.method(), request.params())
                {
                    match result {
                        Ok(init_request) => {
                            tracing::debug!("ensure_initialized: InitializeRequest (agent mode)");
                            let (modified_request, modified_request_cx) = self
                                .lazy_initialize_components(
                                    conductor_tx,
                                    client,
                                    init_request,
                                    false, // proxy_mode
                                    request_cx.cast(),
                                )
                                .await?;
                            let untyped = modified_request.to_untyped_message()?;
                            Ok(Some(MessageCx::Request(
                                untyped,
                                modified_request_cx.erase_to_json(),
                            )))
                        }
                        Err(error) => {
                            request_cx.respond_with_error(error)?;
                            Ok(None)
                        }
                    }
                } else {
                    request_cx.respond_with_error(
                        Error::invalid_request()
                            .with_data("expected `initialize` or `_proxy/initialize` request"),
                    )?;
                    Ok(None)
                }
            }

            MessageCx::Notification(_) => {
                client.send_error_notification(
                    Error::invalid_request()
                        .with_data("expected `initialize` or `_proxy/initialize` request"),
                )?;
                Ok(None)
            }
        }
    }

    async fn lazy_initialize_components(
        &mut self,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        cx: &JrConnectionCx<ConductorToClient>,
        initialize_request: InitializeRequest,
        proxy_mode: bool,
        initialize_request_cx: JrRequestCx<InitializeResponse>,
    ) -> Result<(InitializeRequest, JrRequestCx<InitializeResponse>), sacp::Error> {
        assert!(self.proxies.is_empty());
        assert!(self.agent.is_none());

        info!(
            ?proxy_mode,
            ?initialize_request,
            "lazy_initialize_components"
        );

        let Some(component_list) = self.component_list.take() else {
            return Err(sacp::util::internal_error("no component list"));
        };

        let (modified_req, mut dyn_components) = component_list
            .instantiate_components(initialize_request)
            .await?;

        debug!(
            ?modified_req,
            dyn_components_len = dyn_components.len(),
            "instantiated components"
        );

        // If we are in agent mode, spawn the agent component
        if !proxy_mode {
            let Some(agent_component) = dyn_components.pop() else {
                return Err(sacp::util::internal_error("no agent component"));
            };

            // Spawn the agent component (if any)
            let agent_index = dyn_components.len();
            debug!(agent_index, "spawning agent");
            let agent_cx = cx.spawn_connection(
                ConductorToAgent::builder()
                    .name("conductor-to-agent")
                    // Intercept agent-to-client messages from the agent.
                    .on_receive_message({
                        let mut conductor_tx = conductor_tx.clone();
                        async move |message_cx: MessageCx, _cx| {
                            conductor_tx
                                .send(ConductorMessage::AgentToClient {
                                    source_component_index: SourceComponentIndex::Component(
                                        agent_index,
                                    ),
                                    message: message_cx,
                                })
                                .await
                                .map_err(sacp::util::internal_error)
                        }
                    })
                    .connect_to(agent_component)?,
                |c| Box::pin(c.serve()),
            )?;
            self.agent = Some(agent_cx);
        }

        // Spawn each proxy component
        for (component_index, dyn_component) in dyn_components.into_iter().enumerate() {
            debug!(component_index, "spawning proxy");

            let proxy_cx = cx.spawn_connection(
                ConductorToProxy::builder()
                    .name(format!("conductor-to-component({})", component_index))
                    // Intercept messages sent by a proxy component to its successor.
                    .on_receive_message({
                        let mut conductor_tx = conductor_tx.clone();
                        async move |message_cx: MessageCx<SuccessorMessage, SuccessorMessage>,
                                    _cx| {
                            conductor_tx
                                .send(ConductorMessage::ClientToAgent {
                                    target_component_index: component_index + 1,
                                    message: message_cx.map(|r, cx| (r.message, cx), |n| n.message),
                                })
                                .await
                                .map_err(sacp::util::internal_error)
                        }
                    })
                    // Intercept agent-to-client messages from the proxy.
                    .on_receive_message({
                        let mut conductor_tx = conductor_tx.clone();
                        async move |message_cx: MessageCx<UntypedMessage, UntypedMessage>, _cx| {
                            conductor_tx
                                .send(ConductorMessage::AgentToClient {
                                    source_component_index: SourceComponentIndex::Component(
                                        component_index,
                                    ),
                                    message: message_cx,
                                })
                                .await
                                .map_err(sacp::util::internal_error)
                        }
                    })
                    .connect_to(dyn_component)?,
                |c| Box::pin(c.serve()),
            )?;
            self.proxies.push(proxy_cx);
        }

        info!(
            proxy_count = self.proxies.len(),
            agent_count = self.agent.as_ref().map_or(0, |_| 1),
            proxy_mode,
            "Components spawned"
        );

        Ok((modified_req, initialize_request_cx))
    }

    // Intercept `session/new` requests and replace MCP servers based on `acp:...` URLs with stdio-based servers.
    async fn forward_session_new_request(
        &mut self,
        target_component_index: usize,
        mut request: NewSessionRequest,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        request_cx: JrRequestCx<NewSessionResponse>,
        connection_cx: &JrConnectionCx<ConductorToClient>,
    ) -> Result<(), sacp::Error> {
        // Before forwarding the ACP request to the agent, replace ACP servers with stdio-based servers.
        // Collect oneshot senders for delivering session_id to listeners.
        if self.is_agent_component(target_component_index) {
            for mcp_server in &mut request.mcp_servers {
                self.bridge_listeners
                    .transform_mcp_server(
                        connection_cx,
                        mcp_server,
                        conductor_tx,
                        &self.mcp_bridge_mode,
                    )
                    .await?;
            }

            self.agent
                .as_ref()
                .expect("`is_agent_component` returning true => has an agent")
                .send_request(request)
                .forward_response_via(conductor_tx, request_cx)
        } else {
            self.proxies[target_component_index]
                .send_request(request)
                .forward_response_via(conductor_tx, request_cx)
        }
    }
}

/// Identifies the source of an agent-to-client message.
///
/// This enum handles the fact that the conductor may receive messages from two different sources:
/// 1. From one of its managed components (identified by index)
/// 2. From the conductor's own successor in a larger proxy chain (when in proxy mode)
#[derive(Debug, Clone, Copy)]
pub enum SourceComponentIndex {
    /// Message from the conductor's own successor (only valid in proxy mode).
    ///
    /// When the conductor itself acts as a proxy in a larger chain, it may receive
    /// messages from the next component beyond its managed chain. This variant represents
    /// that case, where the actual component index will be `self.components.len()`.
    ConductorSuccessor,

    /// Message from a specific component at the given index in the managed chain.
    Component(usize),
}

/// Trait for lazy component instantiation based on the Initialize request.
///
/// This trait enables the conductor to defer component selection until after
/// receiving and examining the Initialize request from the upstream client.
/// This allows dynamic proxy chain construction based on client capabilities.
///
/// Implementations return component specifications (things that implement `Component`),
/// and the conductor handles spawning and wiring them together.
///
/// # Examples
///
/// Simple case - provide all components unconditionally:
/// ```ignore
/// let components: Vec<sacp::DynComponent> = vec![
///     sacp::DynComponent::new(AcpAgent::from_str("python proxy.py")?),
///     sacp::DynComponent::new(AcpAgent::from_str("python agent.py")?),
/// ];
/// Conductor::new("my-conductor", components, None)
/// ```
///
/// Dynamic case - examine capabilities and choose components conditionally:
/// ```ignore
/// Conductor::new("my-conductor", |_cx, _conductor_tx, init_req| async move {
///     let needs_auth = init_req.capabilities.contains(&"auth");
///     let mut components: Vec<sacp::DynComponent> = Vec::new();
///     if needs_auth {
///         components.push(sacp::DynComponent::new(AcpAgent::from_str("python auth-proxy.py")?));
///     }
///     components.push(sacp::DynComponent::new(AcpAgent::from_str("python agent.py")?));
///     Ok((init_req, components))
/// }, None)
/// ```
pub trait ComponentList: Send {
    /// Select components based on the Initialize request.
    ///
    /// # Arguments
    ///
    /// * `req` - The Initialize request from the upstream client
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// * The (potentially modified) Initialize request to forward downstream
    /// * A vector of component specifications to be spawned by the conductor
    fn instantiate_components(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<(InitializeRequest, Vec<sacp::DynComponent>), sacp::Error>,
    >;
}

/// Simple implementation: provide all components unconditionally.
impl<T> ComponentList for Vec<T>
where
    T: Component + 'static,
{
    fn instantiate_components(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<(InitializeRequest, Vec<sacp::DynComponent>), sacp::Error>,
    > {
        Box::pin(async move {
            let components: Vec<sacp::DynComponent> = (*self)
                .into_iter()
                .map(|c| sacp::DynComponent::new(c))
                .collect();
            Ok((req, components))
        })
    }
}

/// Dynamic implementation: closure receives the Initialize request and returns components.
impl<F, Fut> ComponentList for F
where
    F: FnOnce(InitializeRequest) -> Fut + Send + 'static,
    Fut: std::future::Future<
            Output = Result<(InitializeRequest, Vec<sacp::DynComponent>), sacp::Error>,
        > + Send
        + 'static,
{
    fn instantiate_components(
        self: Box<Self>,
        req: InitializeRequest,
    ) -> futures::future::BoxFuture<
        'static,
        Result<(InitializeRequest, Vec<sacp::DynComponent>), sacp::Error>,
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
    /// A message (request or notification) targeting a component from its client.
    /// This message will be forwarded "as is" to the component.
    ClientToAgent {
        target_component_index: usize,
        message: MessageCx,
    },

    /// A message (request or notification) sent by a component to its client.
    /// This message will be forwarded "as is" to its client.
    AgentToClient {
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

    /// Forward a response back to a request context.
    ///
    /// This variant avoids a subtle race condition by preserving the
    /// order of responses vis-a-vis notifications and requests. Whenever a new message
    /// from a component arrives, whether it's a new request or a notification, we route
    /// it through the conductor's central message queue.
    ///
    /// The invariant we must ensure in particular is that any requests or notifications
    /// that arrive BEFORE the response will be processed first.
    ForwardResponse {
        request_cx: JrRequestCx<serde_json::Value>,
        result: Result<serde_json::Value, sacp::Error>,
    },
}

trait JrConnectionCxExt {
    fn send_proxied_message_via(
        &self,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        message: MessageCx,
    ) -> Result<(), sacp::Error>;
}

impl<Role: HasDefaultEndpoint + sacp::HasEndpoint<<Role as JrRole>::HandlerEndpoint>>
    JrConnectionCxExt for JrConnectionCx<Role>
{
    fn send_proxied_message_via(
        &self,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        message: MessageCx,
    ) -> Result<(), sacp::Error> {
        match message {
            MessageCx::Request(request, request_cx) => self
                .send_request(request)
                .forward_response_via(conductor_tx, request_cx),
            MessageCx::Notification(notification) => self.send_notification(notification),
        }
    }
}

trait JrRequestCxExt<T: JrResponsePayload> {
    async fn respond_with_result_via(
        self,
        conductor_tx: mpsc::Sender<ConductorMessage>,
        result: Result<T, sacp::Error>,
    ) -> Result<(), sacp::Error>;
}

impl<T: JrResponsePayload> JrRequestCxExt<T> for JrRequestCx<T> {
    async fn respond_with_result_via(
        self,
        mut conductor_tx: mpsc::Sender<ConductorMessage>,
        result: Result<T, sacp::Error>,
    ) -> Result<(), sacp::Error> {
        let result = result.and_then(|response| response.into_json(self.method()));
        conductor_tx
            .send(ConductorMessage::ForwardResponse {
                request_cx: self.erase_to_json(),
                result,
            })
            .await
            .map_err(|e| sacp::util::internal_error(format!("Failed to send response: {}", e)))
    }
}

pub trait JrResponseExt<T: JrResponsePayload> {
    fn forward_response_via(
        self,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        request_cx: JrRequestCx<T>,
    ) -> Result<(), sacp::Error>;
}

impl<T: JrResponsePayload> JrResponseExt<T> for JrResponse<T> {
    fn forward_response_via(
        self,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        request_cx: JrRequestCx<T>,
    ) -> Result<(), sacp::Error> {
        let conductor_tx = conductor_tx.clone();
        self.await_when_result_received(async move |result| {
            request_cx
                .respond_with_result_via(conductor_tx, result)
                .await
        })
    }
}
