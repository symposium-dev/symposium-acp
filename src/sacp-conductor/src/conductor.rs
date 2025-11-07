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
//! 4. Manages the `_meta.symposium.proxy` capability to signal chain position
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
//! ## Capability Management
//!
//! Components discover their position in the chain via the `_meta.symposium.proxy`
//! capability in `initialize` requests:
//!
//! - **First component** (from editor): Receives proxy capability if chain has >1 components
//! - **Middle components**: Receive proxy capability to indicate they have a successor
//! - **Last component**: Does NOT receive proxy capability (talks directly to agent)
//!
//! The conductor manages this by:
//! - Adding proxy capability when editor sends initialize to first component (if chain has >1 components)
//! - Adding proxy capability when component sends initialize to successor (if successor is not last)
//! - Removing proxy capability when component sends initialize to last component
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

use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};

use futures::{
    SinkExt, StreamExt,
    channel::mpsc::{self},
};
use sacp::{
    IntoJrTransport, JrMessageHandler,
    schema::{InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse},
};
use sacp::{
    JrConnectionCx, JrHandlerChain, JrNotification, JrRequest, JrRequestCx, JrResponse,
    MessageAndCx, MetaCapabilityExt, Proxy, UntypedMessage,
    util::{TypeNotification, TypeRequest},
};
use sacp_proxy::{
    AcpProxyExt, McpConnectRequest, McpConnectResponse, McpDisconnectNotification,
    McpOverAcpNotification, McpOverAcpRequest, SuccessorNotification, SuccessorRequest,
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
    providers: Vec<Box<dyn IntoJrTransport>>,
    conductor_command: Option<Vec<String>>,
}

impl Conductor {
    pub fn new(
        name: String,
        providers: Vec<impl IntoJrTransport + 'static>,
        conductor_command: Option<Vec<String>>,
    ) -> Self {
        Conductor {
            name,
            providers: providers
                .into_iter()
                .map(|t| Box::new(t) as Box<dyn IntoJrTransport>)
                .collect(),
            conductor_command,
        }
    }

    pub fn into_handler_chain(self) -> JrHandlerChain<impl JrMessageHandler> {
        let (mut conductor_tx, mut conductor_rx) = mpsc::channel(128 /* chosen arbitrarily */);

        let conductor_command = self.conductor_command.unwrap_or_else(|| {
            let argv0 = std::env::current_exe()
                .expect("valid current executable path")
                .display()
                .to_string();
            vec![argv0]
        });

        let mut handler = ConductorHandler {
            components: Default::default(),
            bridge_listeners: Default::default(),
            bridge_connections: Default::default(),
            conductor_command,
            proxy_mode: AtomicBool::new(false),
        };

        let len_components = self.providers.len();

        JrHandlerChain::new()
            .name(self.name)
            .on_receive_message_from_successor({
                let mut conductor_tx = conductor_tx.clone();
                async move |message: MessageAndCx| {
                    // If we receive a message from our successor, we must be in proxy mode or else something odd is going on.
                    conductor_tx
                        .send(ConductorMessage::AgentToClient {
                            source_component_index: len_components,
                            message,
                        })
                        .await
                        .map_err(sacp::util::internal_error)
                }
            })
            // Any incoming messages from the client are client-to-agent messages targeting the first component.
            .on_receive_message({
                let mut conductor_tx = conductor_tx.clone();
                async move |message: MessageAndCx| {
                    conductor_tx
                        .send(ConductorMessage::ClientToAgent {
                            target_component_index: 0,
                            message,
                        })
                        .await
                        .map_err(sacp::util::internal_error)
                }
            })
            .with_spawned(async move |cx| {
                for provider in self.providers {
                    handler.launch_proxy(&cx, &mut conductor_tx, provider)?;
                }

                // This is the "central actor" of the conductor. Most other things forward messages
                // via `conductor_tx` into this loop. This lets us serialize the conductor's activity.
                while let Some(message) = conductor_rx.next().await {
                    handler
                        .handle_conductor_message(&cx, message, &mut conductor_tx)
                        .await?;
                }
                Ok(())
            })
    }

    /// Convenience method to run the conductor with a transport.
    ///
    /// This is equivalent to:
    /// ```ignore
    /// conductor.into_handler_chain()
    ///     .connect_to(transport)
    ///     .serve()
    ///     .await
    /// ```
    pub async fn run(self, transport: impl IntoJrTransport) -> Result<(), sacp::Error> {
        self.into_handler_chain()
            .connect_to(transport)?
            .serve()
            .await
    }
}

/// The conductor manages the proxy chain lifecycle and message routing.
///
/// It maintains connections to all components in the chain and routes messages
/// bidirectionally between the editor, components, and agent.
///
pub struct ConductorHandler {
    /// Manages the TCP listeners for MCP connections that will be proxied over ACP.
    bridge_listeners: McpBridgeListeners,

    /// Manages active connections to MCP clients.
    bridge_connections: HashMap<String, McpBridgeConnection>,

    /// The chain of spawned components, ordered from first (index 0) to last
    components: Vec<JrConnectionCx>,

    /// Command and args to spawn conductor MCP bridge processes
    /// E.g., vec!["conductor"] or vec!["cargo", "run", "-p", "conductor", "--"]
    conductor_command: Vec<String>,

    /// Whether the conductor is operating in proxy mode.
    /// In proxy mode, the conductor itself acts as a proxy component in a larger chain,
    /// and ALL components (including the last) receive the proxy capability.
    /// Uses AtomicBool for thread-safe interior mutability since we detect this during initialization.
    proxy_mode: AtomicBool,
}

impl ConductorHandler {
    /// Recursively spawns components and builds the proxy chain.
    ///
    /// This function implements the recursive chain building pattern:
    /// 1. Pop the next component from the `providers` list
    /// 2. Create the component (either spawn subprocess or use mock)
    /// 3. Set up JSON-RPC connection and message handlers
    /// 4. Recursively call itself to spawn the next component
    /// 5. When no components remain, start the message routing loop via `serve()`
    ///
    /// Each component is given a channel to send messages back to the conductor,
    /// enabling the bidirectional message routing.
    ///
    /// # Arguments
    ///
    /// - `providers`: Stack of component providers (reversed, so we pop from the end)
    /// - `serve_args`: I/O streams and conductor channel for the serve method
    fn launch_proxy(
        &mut self,
        cx: &JrConnectionCx,
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
        provider: Box<dyn IntoJrTransport>,
    ) -> Result<(), sacp::Error> {
        let component_index = self.components.len();

        info!(component_index, "Creating component");

        let connection = JrHandlerChain::new()
            .name(format!("conductor-to-component({})", component_index))
            // Intercept messages sent by a proxy component (acting as ACP client) to its successor agent.
            .on_receive_message({
                let mut conductor_tx = conductor_tx.clone();
                async move |message_cx: MessageAndCx<
                    SuccessorRequest<UntypedMessage>,
                    SuccessorNotification<UntypedMessage>,
                >| {
                    conductor_tx
                        .send(ConductorMessage::ClientToAgent {
                            target_component_index: component_index + 1,
                            message: message_cx
                                .map(|r, cx| (r.request, cx), |n, cx| (n.notification, cx)),
                        })
                        .await
                        .map_err(sacp::util::internal_error)
                }
            })
            // The proxy sees the conductor as its "client",
            // so if it sends a normal ACP message
            // (i.e., an agent-to-client message),
            // the conductor will forward that
            // to the proxy's predecessor.
            .on_receive_message({
                let mut conductor_tx = conductor_tx.clone();
                async move |message_cx: MessageAndCx<UntypedMessage, UntypedMessage>| {
                    conductor_tx
                        .send(ConductorMessage::AgentToClient {
                            source_component_index: component_index,
                            message: message_cx,
                        })
                        .await
                        .map_err(sacp::util::internal_error)
                }
            })
            .connect_to(provider)?;

        let component_cx = cx.spawn_connection(connection, |c| Box::pin(c.serve()))?;
        self.components.push(component_cx);

        Ok(())
    }

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
    /// * We insert the "proxy" capability to initialization messages going to proxy components (and remove it for the agent component).
    /// * We modify "session/new" requests that use `acp:...` as the URL for an MCP server to redirect
    ///   through a stdio server that runs on localhost and bridges messages.
    async fn handle_conductor_message(
        &mut self,
        client: &JrConnectionCx,
        message: ConductorMessage,
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(?message, "handle_conductor_message");

        match message {
            ConductorMessage::ClientToAgent {
                target_component_index,
                message,
            } => {
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
            } => self.send_message_to_predecessor_of(client, source_component_index, message),

            // New MCP connection request. Send it back along the chain to get a connection id.
            // When the connection id arrives, send a message back into this conductor loop with
            // the connection id and the (as yet unspawned) actor.
            ConductorMessage::McpConnectionReceived {
                acp_url,
                connection,
                actor,
            } => self
                .send_request_to_predecessor_of(
                    client,
                    self.components.len() - 1,
                    McpConnectRequest { acp_url },
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
                }),

            // MCP connection successfully established. Spawn the actor
            // and insert the connection into our map fot future reference.
            ConductorMessage::McpConnectionEstablished {
                response: McpConnectResponse { connection_id },
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
                            McpOverAcpRequest {
                                connection_id: connection_id.clone(),
                                request,
                            },
                            request_cx,
                        )
                    },
                    |notification, notification_cx| {
                        (
                            McpOverAcpNotification {
                                connection_id: connection_id.clone(),
                                notification,
                            },
                            notification_cx,
                        )
                    },
                );
                self.send_message_to_predecessor_of(client, self.components.len() - 1, wrapped)
            }

            // MCP client disconnected. Remove it from our map and send the
            // notification backwards along the chain.
            ConductorMessage::McpConnectionDisconnected { notification } => {
                self.bridge_connections.remove(&notification.connection_id);
                self.send_notification_to_predecessor_of(
                    client,
                    self.components.len() - 1,
                    notification,
                )
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
        client: &JrConnectionCx,
        source_component_index: usize,
        message: MessageAndCx<Req, N>,
    ) -> Result<(), sacp::Error>
    where
        Req::Response: Send,
    {
        if source_component_index == 0 {
            client.send_proxied_message(message)
        } else {
            // If message is coming from the conductor's successor,
            // check whether we have proxy capability and error otherwise.
            if source_component_index == self.components.len() {
                if !self.proxy_mode.load(Ordering::Relaxed) {
                    return Err(sacp::Error::invalid_request().with_data("cannot accept successor message when not initialized with proxy capability"));
                }
            }

            let wrapped = message.map(
                |request, request_cx| (SuccessorRequest { request }, request_cx),
                |notification, notification_cx| {
                    (SuccessorNotification { notification }, notification_cx)
                },
            );
            self.components[source_component_index - 1].send_proxied_message(wrapped)
        }
    }

    fn send_request_to_predecessor_of<Req: JrRequest>(
        &mut self,
        client: &JrConnectionCx,
        source_component_index: usize,
        request: Req,
    ) -> JrResponse<Req::Response> {
        if source_component_index == 0 {
            client.send_request(request)
        } else {
            self.components[source_component_index - 1].send_request(SuccessorRequest { request })
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
        client: &JrConnectionCx,
        component_index: usize,
        notification: N,
    ) -> Result<(), sacp::Error> {
        if component_index == 0 {
            client.send_notification(notification)
        } else {
            self.components[component_index - 1]
                .send_notification(SuccessorNotification { notification })
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
        message: MessageAndCx,
        client: &JrConnectionCx,
    ) -> Result<(), sacp::Error> {
        // In proxy mode, if the target is beyond our component chain,
        // forward to the conductor's own successor (via client connection)
        if self.proxy_mode.load(Ordering::Relaxed)
            && target_component_index >= self.components.len()
        {
            debug!(
                target_component_index,
                component_count = self.components.len(),
                "Proxy mode: forwarding successor message to conductor's successor"
            );
            // Wrap the message as a successor message before sending
            let to_successor_message = message.map(
                |request, request_cx| (SuccessorRequest { request }, request_cx),
                |notification, notification_cx| {
                    (SuccessorNotification { notification }, notification_cx)
                },
            );
            return client.send_proxied_message(to_successor_message);
        }

        tracing::debug!(?message, "forward_client_to_agent_message");

        match message {
            MessageAndCx::Request(request, request_cx) => {
                self.forward_client_to_agent_request(
                    conductor_tx,
                    target_component_index,
                    request,
                    request_cx,
                )
                .await
            }
            MessageAndCx::Notification(notification, _cx) => {
                self.send_client_to_agent_notification(target_component_index, notification, client)
                    .await
            }
        }
    }

    /// Send a request from 'left to right', forwarding the reply
    /// to `request_cx`. Left-to-right means from the client or an
    /// intermediate proxy to the component at `target_component_index` (could be
    /// a proxy or the agent). Makes changes to select messages
    /// along the way (e.g., `initialize` and `session/new`).
    async fn forward_client_to_agent_request(
        &mut self,
        conductor_tx: &mut mpsc::Sender<ConductorMessage>,
        target_component_index: usize,
        request: UntypedMessage,
        request_cx: JrRequestCx<serde_json::Value>,
    ) -> Result<(), sacp::Error> {
        TypeRequest::new(request, request_cx)
            .handle_if(async |request: InitializeRequest, request_cx| {
                // When forwarding "initialize", we either add or remove the proxy capability,
                // depending on whether we are sending this message to the final component.
                self.forward_initialize_request(target_component_index, request, request_cx)
            })
            .await
            .handle_if(async |request: NewSessionRequest, request_cx| {
                // When forwarding "session/new", we adjust MCP servers to manage "acp:" URLs.
                self.forward_session_new_request(
                    target_component_index,
                    request,
                    &conductor_tx,
                    request_cx,
                )
                .await
            })
            .await
            .handle_if(
                async |request: McpOverAcpRequest<UntypedMessage>, request_cx| {
                    let McpOverAcpRequest {
                        connection_id,
                        request: mcp_request,
                    } = request;
                    self.bridge_connections
                        .get_mut(&connection_id)
                        .ok_or_else(|| {
                            sacp::util::internal_error(format!(
                                "unknown connection id: {}",
                                connection_id
                            ))
                        })?
                        .send(MessageAndCx::Request(mcp_request, request_cx))
                        .await
                },
            )
            .await
            .otherwise(async |request: UntypedMessage, request_cx| {
                // Handle other types of requests here
                // Otherwise, just send the message along "as is".
                self.components[target_component_index]
                    .send_request(request)
                    .forward_to_request_cx(request_cx)
            })
            .await
    }

    /// Send a notification from 'left to right'.
    /// Left-to-right means from the client or an intermediate proxy to the component
    /// at `target_component_index` (could be a proxy or the agent).
    async fn send_client_to_agent_notification(
        &mut self,
        target_component_index: usize,
        notification: UntypedMessage,
        cx: &JrConnectionCx,
    ) -> Result<(), sacp::Error> {
        let cx_clone = cx.clone();
        TypeNotification::new(notification, cx)
            .handle_if(
                async |notification: McpOverAcpNotification<UntypedMessage>| {
                    let McpOverAcpNotification {
                        connection_id,
                        notification: mcp_notification,
                    } = notification;
                    self.bridge_connections
                        .get_mut(&connection_id)
                        .ok_or_else(|| {
                            sacp::util::internal_error(format!(
                                "unknown connection id: {}",
                                connection_id
                            ))
                        })?
                        .send(MessageAndCx::Notification(mcp_notification, cx_clone))
                        .await
                },
            )
            .await
            .otherwise(async |notification| {
                // Otherwise, just send the message along "as is".
                self.components[target_component_index].send_notification(notification)
            })
            .await
    }

    /// Checks if the given component index is the agent.
    ///
    /// Note that, in proxy mode, there is no agent.
    fn is_agent_component(&self, component_index: usize) -> bool {
        !self.proxy_mode.load(Ordering::Relaxed) && component_index == self.components.len() - 1
    }

    /// Checks if the given component index is the last proxy before the agent.
    fn forward_initialize_request(
        &self,
        target_component_index: usize,
        mut initialize_req: InitializeRequest,
        request_cx: JrRequestCx<InitializeResponse>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(?initialize_req, "forward_initialize_request");

        // Handle proxy capability in incoming initialize request
        let initialize_had_proxy = initialize_req.has_meta_capability(Proxy);
        if initialize_had_proxy {
            if target_component_index == 0 {
                // First component receiving proxy capability means conductor is in proxy mode
                debug!("Conductor entering proxy mode - received initialize with proxy capability");
                self.proxy_mode.store(true, Ordering::Relaxed);

                // Remove the proxy capability from the request before forwarding
                initialize_req = initialize_req.remove_meta_capability(Proxy);
            } else {
                // Components should never forward initialize with proxy capability attached
                return Err(sacp::util::internal_error(
                    "conductor received unexpected initialization request with proxy capability",
                ));
            }
        }

        // In normal mode, only non-agent components get proxy capability.
        // In proxy mode, ALL components (including the last) get proxy capability.
        let is_agent = self.is_agent_component(target_component_index);

        tracing::debug!(?is_agent, "forward_initialize_request");

        if !is_agent {
            // Add proxy capability and verify response
            initialize_req = initialize_req.add_meta_capability(Proxy);
            self.components[target_component_index]
                .send_request(initialize_req)
                .await_when_result_received(async move |response| match response {
                    Ok(mut response) => {
                        // Verify proxy capability handshake
                        // Each proxy component must respond with Proxy capability or we
                        // abort the conductor.
                        if !response.has_meta_capability(Proxy) {
                            return Err(sacp::util::internal_error(format!(
                                "component {} is not a proxy",
                                target_component_index
                            )));
                        }

                        // We don't want to respond with that proxy capability to the predecessor.
                        // Proxy communication is just between the conductor and others.
                        if !initialize_had_proxy {
                            response = response.remove_meta_capability(Proxy);
                        }

                        request_cx.respond(response)
                    }
                    Err(error) => request_cx.respond_with_error(error),
                })
        } else {
            // Agent component - no proxy capability
            self.components[target_component_index]
                .send_request(initialize_req)
                .await_when_result_received(async move |response| {
                    tracing::debug!(?response, "got initialize response");

                    match response {
                        Ok(response) => request_cx.respond(response),
                        Err(error) => request_cx.respond_with_error(error),
                    }
                })
        }
    }

    // Intercept `session/new` requests and replace MCP servers based on `acp:...` URLs with stdio-based servers.
    async fn forward_session_new_request(
        &mut self,
        target_component_index: usize,
        mut request: NewSessionRequest,
        conductor_tx: &mpsc::Sender<ConductorMessage>,
        request_cx: JrRequestCx<NewSessionResponse>,
    ) -> Result<(), sacp::Error> {
        // Before forwarding the ACP request to the agent, replace ACP servers with stdio-based servers.
        if self.is_agent_component(target_component_index) {
            for mcp_server in &mut request.mcp_servers {
                self.bridge_listeners
                    .transform_mcp_servers(
                        &request_cx,
                        mcp_server,
                        conductor_tx,
                        &self.conductor_command,
                    )
                    .await?;
            }
        }

        self.components[target_component_index]
            .send_request(request)
            .forward_to_request_cx(request_cx)
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
        message: MessageAndCx,
    },

    /// A message (request or notification) sent by a component to its client.
    /// This message will be forwarded "as is" to its client.
    AgentToClient {
        source_component_index: usize,
        message: MessageAndCx,
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
        message: MessageAndCx,
    },

    /// Message sent when MCP client disconnects
    McpConnectionDisconnected {
        notification: McpDisconnectNotification,
    },
}
