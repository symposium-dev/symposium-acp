//! Cookbook of common patterns for building ACP components.
//!
//! This module contains guides for the three main things you can build with sacp:
//!
//! - **Clients** - Connect to an existing agent and send prompts
//! - **Proxies** - Sit between client and agent to add capabilities (like MCP tools)
//! - **Agents** - Respond to prompts with AI-powered responses
//!
//! # Building Clients
//!
//! A client connects to an agent, sends requests, and handles responses. Use
//! [`ClientToAgent`] as your link type.
//!
//! - [`connecting_as_client`] - Basic pattern for sending requests
//!
//! # Building Proxies
//!
//! A proxy sits between client and agent, intercepting and optionally modifying
//! messages. The most common use case is adding MCP tools. Use [`ProxyToConductor`]
//! as your link type.
//!
//! **Important:** Proxies don't run standalone—they need the [`sacp-conductor`] to
//! orchestrate the connection between client, proxies, and agent. See
//! [`running_proxies_with_conductor`] for how to put the pieces together.
//!
//! - [`global_mcp_server`] - Add tools that work across all sessions
//! - [`per_session_mcp_server`] - Add tools with session-specific state
//! - [`reusable_components`] - Package your proxy as a [`Component`] for composition
//! - [`running_proxies_with_conductor`] - Run your proxy with an agent
//!
//! [`sacp-conductor`]: https://crates.io/crates/sacp-conductor
//!
//! # Building Agents
//!
//! An agent receives prompts and generates responses. Use [`AgentToClient`] as
//! your link type.
//!
//! - [`building_an_agent`] - Handle initialization, sessions, and prompts
//! - [`reusable_components`] - Package your agent as a [`Component`]
//! - [`custom_message_handlers`] - Fine-grained control over message routing
//!
//! # Roles and Peers
//!
//! ACP connections are typed by their *link*, which captures both "who I am"
//! and "who I'm talking to". Links implement [`JrLink`] and determine what
//! operations are valid on a connection.
//!
//! ## Peers
//!
//! *Peers* ([`JrPeer`]) are logical destinations for messages:
//!
//! - [`ClientPeer`] - The client (IDE, CLI, etc.)
//! - [`AgentPeer`] - The agent (AI-powered component)
//!
//! The crate also defines [`ConductorPeer`] and [`ProxyPeer`], but these are
//! used internally to manage proxy orchestration; you shouldn't need them
//! during typical usage.
//!
//! Most links have a single implicit peer, but proxies can send to
//! multiple peers. Use [`send_request_to`] and [`send_notification_to`]
//! to specify the destination explicitly.
//!
//! ## Link Types
//!
//! The built-in link types are:
//!
//! | Link | Description | Can send to |
//! |------|-------------|-------------|
//! | [`ClientToAgent`] | Client's connection to an agent | `AgentPeer` |
//! | [`AgentToClient`] | Agent's connection to a client | `ClientPeer` |
//! | [`ProxyToConductor`] | Proxy's connection to the conductor | `ClientPeer`, `AgentPeer` |
//! | [`ConductorToClient`] | Conductor's connection to a client | `ClientPeer`, `AgentPeer` |
//! | [`ConductorToProxy`] | Conductor's connection to a proxy | `AgentPeer` |
//! | [`ConductorToAgent`] | Conductor's connection to the final agent | `AgentPeer` |
//!
//! ## Proxies and Multiple Peers
//!
//! A proxy sits between client and agent, so it needs to send messages in
//! both directions. [`ProxyToConductor`] implements `HasPeer<ClientPeer>` and
//! `HasPeer<AgentPeer>`, allowing it to forward messages appropriately:
//!
//! ```ignore
//! // Forward a request toward the agent
//! cx.send_request_to(AgentPeer, request).forward_to_request_cx(request_cx)?;
//!
//! // Send a notification toward the client
//! cx.send_notification_to(ClientPeer, notification)?;
//! ```
//!
//! When sending to `AgentPeer` from a proxy, messages are automatically wrapped
//! in [`SuccessorMessage`] envelopes. When receiving from `AgentPeer`, they're
//! automatically unwrapped.
//!
//! [`JrLink`]: crate::link::JrLink
//! [`JrPeer`]: crate::peer::JrPeer
//! [`ClientPeer`]: crate::ClientPeer
//! [`AgentPeer`]: crate::AgentPeer
//! [`ConductorPeer`]: crate::ConductorPeer
//! [`ProxyPeer`]: crate::peer::ProxyPeer
//! [`ClientToAgent`]: crate::ClientToAgent
//! [`AgentToClient`]: crate::AgentToClient
//! [`ProxyToConductor`]: crate::ProxyToConductor
//! [`ConductorToClient`]: crate::link::ConductorToClient
//! [`ConductorToProxy`]: crate::link::ConductorToProxy
//! [`ConductorToAgent`]: crate::link::ConductorToAgent
//! [`SuccessorMessage`]: crate::schema::SuccessorMessage
//! [`send_request_to`]: crate::JrConnectionCx::send_request_to
//! [`send_notification_to`]: crate::JrConnectionCx::send_notification_to
//! [`Component`]: crate::Component

pub mod connecting_as_client {
    //! Pattern: Connecting as a client.
    //!
    //! To connect to an ACP agent and send requests, use [`run_until`].
    //! This runs your code while the connection handles incoming messages
    //! in the background.
    //!
    //! # Basic Example
    //!
    //! ```
    //! use sacp::{ClientToAgent, AgentToClient, Component};
    //! use sacp::schema::InitializeRequest;
    //!
    //! async fn connect_to_agent(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
    //!     ClientToAgent::builder()
    //!         .name("my-client")
    //!         .run_until(transport, async |cx| {
    //!             // Initialize the connection
    //!             cx.send_request(InitializeRequest {
    //!                 protocol_version: Default::default(),
    //!                 client_capabilities: Default::default(),
    //!                 client_info: None,
    //!                 meta: None,
    //!             }).block_task().await?;
    //!
    //!             // Create a session and send a prompt
    //!             cx.build_session_cwd()?
    //!                 .block_task()
    //!                 .run_until(async |mut session| {
    //!                     session.send_prompt("Hello, agent!")?;
    //!                     let response = session.read_to_string().await?;
    //!                     println!("Agent said: {}", response);
    //!                     Ok(())
    //!                 })
    //!                 .await
    //!         })
    //!         .await
    //! }
    //! ```
    //!
    //! # Using the Session Builder
    //!
    //! The [`build_session`] method creates a [`SessionBuilder`] that handles
    //! session creation and provides convenient methods for interacting with
    //! the session:
    //!
    //! - [`send_prompt`] - Send a text prompt to the agent
    //! - [`read_update`] - Read the next update (text chunk, tool call, etc.)
    //! - [`read_to_string`] - Read all text until the turn ends
    //!
    //! The session builder also supports adding MCP servers with [`with_mcp_server`].
    //!
    //! # Handling Permission Requests
    //!
    //! Agents may send [`RequestPermissionRequest`] to ask for user approval
    //! before taking actions. Handle these with [`on_receive_request`]:
    //!
    //! ```ignore
    //! ClientToAgent::builder()
    //!     .on_receive_request(async |req: RequestPermissionRequest, request_cx, _cx| {
    //!         // Auto-approve by selecting the first option (YOLO mode)
    //!         let option_id = req.options.first().map(|opt| opt.id.clone());
    //!         request_cx.respond(RequestPermissionResponse {
    //!             outcome: match option_id {
    //!                 Some(id) => RequestPermissionOutcome::Selected { option_id: id },
    //!                 None => RequestPermissionOutcome::Cancelled,
    //!             },
    //!             meta: None,
    //!         })
    //!     }, sacp::on_receive_request!())
    //!     .run_until(transport, async |cx| { /* ... */ })
    //!     .await
    //! ```
    //!
    //! # Note on `block_task`
    //!
    //! Using [`block_task`] is safe inside `run_until` because the closure runs
    //! as a spawned task, not on the event loop. The event loop continues processing
    //! messages (including the response you're waiting for) while your task blocks.
    //!
    //! [`run_until`]: crate::JrConnectionBuilder::run_until
    //! [`block_task`]: crate::JrResponse::block_task
    //! [`build_session`]: crate::JrConnectionCx::build_session
    //! [`SessionBuilder`]: crate::SessionBuilder
    //! [`send_prompt`]: crate::ActiveSession::send_prompt
    //! [`read_update`]: crate::ActiveSession::read_update
    //! [`read_to_string`]: crate::ActiveSession::read_to_string
    //! [`with_mcp_server`]: crate::SessionBuilder::with_mcp_server
    //! [`RequestPermissionRequest`]: crate::schema::RequestPermissionRequest
    //! [`on_receive_request`]: crate::JrConnectionBuilder::on_receive_request
}

pub mod building_an_agent {
    //! Pattern: Building an agent.
    //!
    //! An agent handles prompts and generates responses. At minimum, an agent must:
    //!
    //! 1. Handle [`InitializeRequest`] to establish the connection
    //! 2. Handle [`NewSessionRequest`] to create sessions
    //! 3. Handle [`PromptRequest`] to process prompts
    //!
    //! Use [`AgentToClient`] as your link type.
    //!
    //! # Minimal Example
    //!
    //! ```
    //! use sacp::{AgentToClient, Component, MessageCx, JrConnectionCx};
    //! use sacp::link::JrLink;
    //! use sacp::schema::{
    //!     InitializeRequest, InitializeResponse, AgentCapabilities,
    //!     NewSessionRequest, NewSessionResponse,
    //!     PromptRequest, PromptResponse, StopReason,
    //! };
    //!
    //! async fn run_agent(transport: impl Component<sacp::ClientToAgent>) -> Result<(), sacp::Error> {
    //!     AgentToClient::builder()
    //!         .name("my-agent")
    //!         // Handle initialization
    //!         .on_receive_request(async |req: InitializeRequest, request_cx, _cx| {
    //!             request_cx.respond(InitializeResponse {
    //!                 protocol_version: req.protocol_version,
    //!                 agent_capabilities: AgentCapabilities::default(),
    //!                 auth_methods: vec![],
    //!                 agent_info: None,
    //!                 meta: None,
    //!             })
    //!         }, sacp::on_receive_request!())
    //!         // Handle session creation
    //!         .on_receive_request(async |req: NewSessionRequest, request_cx, _cx| {
    //!             request_cx.respond(NewSessionResponse {
    //!                 session_id: "session-1".into(),
    //!                 modes: None,
    //!                 meta: None,
    //!             })
    //!         }, sacp::on_receive_request!())
    //!         // Handle prompts
    //!         .on_receive_request(async |req: PromptRequest, request_cx, cx| {
    //!             // Send streaming updates via notifications
    //!             // cx.send_notification(SessionNotification { ... })?;
    //!
    //!             // Return final response
    //!             request_cx.respond(PromptResponse {
    //!                 stop_reason: StopReason::EndTurn,
    //!                 meta: None,
    //!             })
    //!         }, sacp::on_receive_request!())
    //!         // Reject unknown messages
    //!         .on_receive_message(async |message: MessageCx, cx: JrConnectionCx<AgentToClient>| {
    //!             message.respond_with_error(sacp::Error::method_not_found(), cx)
    //!         }, sacp::on_receive_message!())
    //!         .serve(transport)
    //!         .await
    //! }
    //! ```
    //!
    //! # Streaming Responses
    //!
    //! To stream text or other updates to the client, send [`SessionNotification`]s
    //! while processing a prompt:
    //!
    //! ```ignore
    //! .on_receive_request(async |req: PromptRequest, request_cx, cx| {
    //!     // Stream some text
    //!     cx.send_notification(SessionNotification {
    //!         session_id: req.session_id.clone(),
    //!         update: SessionUpdate::Text(TextUpdate {
    //!             text: "Hello, ".into(),
    //!             // ...
    //!         }),
    //!         meta: None,
    //!     })?;
    //!
    //!     cx.send_notification(SessionNotification {
    //!         session_id: req.session_id.clone(),
    //!         update: SessionUpdate::Text(TextUpdate {
    //!             text: "world!".into(),
    //!             // ...
    //!         }),
    //!         meta: None,
    //!     })?;
    //!
    //!     request_cx.respond(PromptResponse {
    //!         stop_reason: StopReason::EndTurn,
    //!         meta: None,
    //!     })
    //! }, sacp::on_receive_request!())
    //! ```
    //!
    //! # Requesting Permissions
    //!
    //! Before taking actions that require user approval (like running commands
    //! or writing files), send a [`RequestPermissionRequest`]:
    //!
    //! ```ignore
    //! let response = cx.send_request(RequestPermissionRequest {
    //!     session_id: session_id.clone(),
    //!     action: PermissionAction::Bash { command: "rm -rf /".into() },
    //!     options: vec![
    //!         PermissionOption { id: "allow".into(), label: "Allow".into() },
    //!         PermissionOption { id: "deny".into(), label: "Deny".into() },
    //!     ],
    //!     meta: None,
    //! }).block_task().await?;
    //!
    //! match response.outcome {
    //!     RequestPermissionOutcome::Selected { option_id } if option_id == "allow" => {
    //!         // User approved, proceed with action
    //!     }
    //!     _ => {
    //!         // User denied or cancelled
    //!     }
    //! }
    //! ```
    //!
    //! # As a Reusable Component
    //!
    //! For agents that will be composed with proxies, implement [`Component`].
    //! See [`reusable_components`] for the pattern.
    //!
    //! [`InitializeRequest`]: crate::schema::InitializeRequest
    //! [`NewSessionRequest`]: crate::schema::NewSessionRequest
    //! [`PromptRequest`]: crate::schema::PromptRequest
    //! [`SessionNotification`]: crate::schema::SessionNotification
    //! [`RequestPermissionRequest`]: crate::schema::RequestPermissionRequest
    //! [`AgentToClient`]: crate::AgentToClient
    //! [`Component`]: crate::Component
    //! [`reusable_components`]: super::reusable_components
}

pub mod reusable_components {
    //! Pattern: Defining reusable components.
    //!
    //! When building agents or proxies that will be composed together (for example,
    //! with [`sacp-conductor`]), define a struct that implements [`Component`].
    //! This allows your component to be connected to other components in a type-safe way.
    //!
    //! # Example
    //!
    //! ```
    //! use sacp::{Component, AgentToClient};
    //! use sacp::link::JrLink;
    //! use sacp::schema::{
    //!     InitializeRequest, InitializeResponse, AgentCapabilities,
    //! };
    //!
    //! struct MyAgent {
    //!     name: String,
    //! }
    //!
    //! impl Component<AgentToClient> for MyAgent {
    //!     async fn serve(self, client: impl Component<<AgentToClient as JrLink>::ConnectsTo>) -> Result<(), sacp::Error> {
    //!         AgentToClient::builder()
    //!             .name(&self.name)
    //!             .on_receive_request(async move |req: InitializeRequest, request_cx, _cx| {
    //!                 request_cx.respond(InitializeResponse {
    //!                     protocol_version: req.protocol_version,
    //!                     agent_capabilities: AgentCapabilities::default(),
    //!                     auth_methods: vec![],
    //!                     agent_info: None,
    //!                     meta: None,
    //!                 })
    //!             }, sacp::on_receive_request!())
    //!             .serve(client)
    //!             .await
    //!     }
    //! }
    //!
    //! let agent = MyAgent { name: "my-agent".into() };
    //! ```
    //!
    //! # Important: Don't block the event loop
    //!
    //! Message handlers run on the event loop. Blocking in a handler prevents the
    //! connection from processing new messages. For expensive work:
    //!
    //! - Use [`JrConnectionCx::spawn`] to offload work to a background task
    //! - Use [`on_receiving_result`] to schedule work when a response arrives
    //!
    //! [`Component`]: crate::Component
    //! [`JrConnectionCx::spawn`]: crate::JrConnectionCx::spawn
    //! [`on_receiving_result`]: crate::JrResponse::on_receiving_result
    //! [`sacp-conductor`]: https://crates.io/crates/sacp-conductor
}

pub mod custom_message_handlers {
    //! Pattern: Custom message handlers.
    //!
    //! For reusable message handling logic, implement [`JrMessageHandler`] and use
    //! [`MatchMessage`] or [`MatchMessageFrom`] for type-safe dispatching.
    //!
    //! This is useful when you need to:
    //! - Share message handling logic across multiple components
    //! - Build complex routing logic that doesn't fit the builder pattern
    //! - Integrate with existing handler infrastructure
    //!
    //! # Example
    //!
    //! ```
    //! use sacp::{JrMessageHandler, MessageCx, Handled, JrConnectionCx};
    //! use sacp::schema::{InitializeRequest, InitializeResponse, AgentCapabilities};
    //! use sacp::util::MatchMessage;
    //!
    //! struct MyHandler;
    //!
    //! impl JrMessageHandler for MyHandler {
    //!     type Link = sacp::link::UntypedLink;
    //!
    //!     async fn handle_message(
    //!         &mut self,
    //!         message: MessageCx,
    //!         _cx: JrConnectionCx<Self::Link>,
    //!     ) -> Result<Handled<MessageCx>, sacp::Error> {
    //!         MatchMessage::new(message)
    //!             .if_request(async |req: InitializeRequest, request_cx| {
    //!                 request_cx.respond(InitializeResponse {
    //!                     protocol_version: req.protocol_version,
    //!                     agent_capabilities: AgentCapabilities::default(),
    //!                     auth_methods: vec![],
    //!                     agent_info: None,
    //!                     meta: None,
    //!                 })
    //!             })
    //!             .await
    //!             .done()
    //!     }
    //!
    //!     fn describe_chain(&self) -> impl std::fmt::Debug {
    //!         "MyHandler"
    //!     }
    //! }
    //! ```
    //!
    //! # When to use `MatchMessage` vs `MatchMessageFrom`
    //!
    //! - [`MatchMessage`] - Use when you don't need peer-aware handling
    //! - [`MatchMessageFrom`] - Use in proxies where messages come from different
    //!   peers (`ClientPeer` vs `AgentPeer`) and may need different handling
    //!
    //! [`JrMessageHandler`]: crate::JrMessageHandler
    //! [`MatchMessage`]: crate::util::MatchMessage
    //! [`MatchMessageFrom`]: crate::util::MatchMessageFrom
}

pub mod global_mcp_server {
    //! Pattern: Global MCP server in handler chain.
    //!
    //! Use this pattern when you want a single MCP server that handles tool calls
    //! for all sessions. The server is added to the connection's handler chain and
    //! automatically injects itself into every `NewSessionRequest` that passes through.
    //!
    //! # When to use
    //!
    //! - The MCP server provides stateless tools (no per-session state needed)
    //! - You want the simplest setup with minimal boilerplate
    //! - Tools don't need access to session-specific context
    //!
    //! # Example
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::{Component, JrResponder, ProxyToConductor};
    //! use schemars::JsonSchema;
    //! use serde::{Deserialize, Serialize};
    //!
    //! #[derive(Debug, Deserialize, JsonSchema)]
    //! struct EchoParams { message: String }
    //!
    //! #[derive(Debug, Serialize, JsonSchema)]
    //! struct EchoOutput { echoed: String }
    //!
    //! // Build the MCP server with tools
    //! let mcp_server = McpServer::builder("my-tools")
    //!     .tool_fn("echo", "Echoes the input",
    //!         async |params: EchoParams, _cx| {
    //!             Ok(EchoOutput { echoed: params.message })
    //!         },
    //!         sacp::tool_fn!())
    //!     .build();
    //!
    //! // The proxy component is generic over the MCP server's responder type
    //! struct MyProxy<R> {
    //!     mcp_server: McpServer<ProxyToConductor, R>,
    //! }
    //!
    //! impl<R: JrResponder<ProxyToConductor> + Send + 'static> Component<ProxyToConductor> for MyProxy<R> {
    //!     async fn serve(self, client: impl Component<sacp::link::ConductorToProxy>) -> Result<(), sacp::Error> {
    //!         ProxyToConductor::builder()
    //!             .with_mcp_server(self.mcp_server)
    //!             .serve(client)
    //!             .await
    //!     }
    //! }
    //!
    //! let proxy = MyProxy { mcp_server };
    //! ```
    //!
    //! # How it works
    //!
    //! When you call [`with_mcp_server`], the MCP server is added as a message
    //! handler. It:
    //!
    //! 1. Intercepts `NewSessionRequest` messages and adds its `acp:UUID` URL to the
    //!    request's `mcp_servers` list
    //! 2. Passes the modified request through to the next handler
    //! 3. Handles incoming MCP protocol messages (tool calls, etc.) for its URL
    //!
    //! [`with_mcp_server`]: crate::JrConnectionBuilder::with_mcp_server
}

pub mod per_session_mcp_server {
    //! Pattern: Per-session MCP server.
    //!
    //! Use this pattern when each session needs its own MCP server instance,
    //! typically because tools need access to session-specific state or context.
    //!
    //! # When to use
    //!
    //! - Tools need access to the session ID or session-specific state
    //! - You want to customize the MCP server based on session parameters
    //! - Tools need to send notifications back to a specific session
    //!
    //! # Simple example: proxy everything
    //!
    //! Use [`start_session_proxy`] when you just want to inject an MCP server
    //! and proxy all messages without any additional processing:
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::schema::NewSessionRequest;
    //! use sacp::{AgentPeer, ClientPeer, Component, ProxyToConductor};
    //! use sacp::link::ConductorToProxy;
    //!
    //! async fn run_proxy(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
    //!     ProxyToConductor::builder()
    //!         .on_receive_request_from(ClientPeer, async |request: NewSessionRequest, request_cx, cx| {
    //!             let cwd = request.cwd.clone();
    //!             let mcp_server = McpServer::builder("session-tools")
    //!                 .tool_fn("get_cwd", "Returns session working directory",
    //!                     async move |_params: (), _cx| {
    //!                         Ok(cwd.display().to_string())
    //!                     }, sacp::tool_fn!())
    //!                 .build();
    //!
    //!             let _session_id = cx.build_session_from(request)
    //!                 .with_mcp_server(mcp_server)?
    //!                 .block_task()
    //!                 .start_session_proxy(request_cx)
    //!                 .await?;
    //!             Ok(())
    //!         }, sacp::on_receive_request!())
    //!         .serve(transport)
    //!         .await
    //! }
    //! ```
    //!
    //! # Advanced example: intercept before proxying
    //!
    //! Use [`start_session`] + [`proxy_remaining_messages`] when you need to
    //! do something with the session before handing off to proxy mode:
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::schema::NewSessionRequest;
    //! use sacp::{AgentPeer, ClientPeer, Component, ProxyToConductor};
    //! use sacp::link::ConductorToProxy;
    //!
    //! async fn run_proxy(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
    //!     ProxyToConductor::builder()
    //!         .on_receive_request_from(ClientPeer, async |request: NewSessionRequest, request_cx, cx| {
    //!             let cwd = request.cwd.clone();
    //!             let mcp_server = McpServer::builder("session-tools")
    //!                 .tool_fn("get_cwd", "Returns session working directory",
    //!                     async move |_params: (), _cx| {
    //!                         Ok(cwd.display().to_string())
    //!                     }, sacp::tool_fn!())
    //!                 .build();
    //!
    //!             let active_session = cx.build_session_from(request)
    //!                 .with_mcp_server(mcp_server)?
    //!                 .block_task()
    //!                 .start_session()
    //!                 .await?;
    //!
    //!             // Do something with the session before proxying...
    //!             tracing::info!(session_id = %active_session.session_id(), "Session created");
    //!
    //!             // Respond to the client and proxy remaining messages
    //!             request_cx.respond(active_session.response())?;
    //!             active_session.proxy_remaining_messages()
    //!         }, sacp::on_receive_request!())
    //!         .serve(transport)
    //!         .await
    //! }
    //! ```
    //!
    //! # How it works
    //!
    //! When you call [`SessionBuilder::with_mcp_server`]:
    //!
    //! 1. The MCP server is converted into a dynamic handler via `into_dynamic_handler()`
    //! 2. The handler is registered for the session's message routing
    //! 3. The MCP server's URL is added to the `NewSessionRequest`
    //! 4. The handler lives as long as the session (dropped when `run_until` completes)
    //!
    //! [`start_session_proxy`]: crate::SessionBuilder::start_session_proxy
    //! [`start_session`]: crate::SessionBuilder::start_session
    //! [`proxy_remaining_messages`]: crate::ActiveSession::proxy_remaining_messages
    //! [`SessionBuilder::with_mcp_server`]: crate::SessionBuilder::with_mcp_server
}

pub mod running_proxies_with_conductor {
    //! Pattern: Running proxies with the conductor.
    //!
    //! Proxies don't run standalone. To add an MCP server (or other proxy behavior)
    //! to an existing agent, you need the **conductor** to orchestrate the connection.
    //!
    //! The conductor:
    //! 1. Accepts connections from clients
    //! 2. Chains your proxies together
    //! 3. Connects to the final agent
    //! 4. Routes messages through the entire chain
    //!
    //! # Using the `sacp-conductor` binary
    //!
    //! The simplest way to run a proxy is with the [`sacp-conductor`] binary.
    //! Configure it with a JSON file:
    //!
    //! ```json
    //! {
    //!   "proxies": [
    //!     { "command": ["cargo", "run", "--bin", "my-proxy"] }
    //!   ],
    //!   "agent": { "command": ["claude-code", "--agent"] }
    //! }
    //! ```
    //!
    //! Then run:
    //!
    //! ```bash
    //! sacp-conductor --config conductor.json
    //! ```
    //!
    //! # Using the conductor as a library
    //!
    //! For more control, use [`sacp-conductor`] as a library with the [`Conductor`] type:
    //!
    //! ```ignore
    //! use sacp_conductor::{Conductor, ProxiesAndAgent};
    //!
    //! // Define your proxy as a Component<ProxyToConductor>
    //! let my_proxy = MyProxy::new();
    //!
    //! // Spawn the agent process
    //! let agent_process = sacp_tokio::spawn_process("claude-code", &["--agent"]).await?;
    //!
    //! // Create the conductor with your proxy chain
    //! let conductor = Conductor::new(ProxiesAndAgent {
    //!     proxies: vec![Box::new(my_proxy)],
    //!     agent: agent_process,
    //! });
    //!
    //! // Run the conductor (it will accept client connections on stdin/stdout)
    //! conductor.serve(client_transport).await?;
    //! ```
    //!
    //! # Why can't I just connect my proxy directly to an agent?
    //!
    //! ACP uses a message envelope format for proxy chains. When a proxy sends a
    //! message toward the agent, it gets wrapped in a [`SuccessorMessage`] envelope.
    //! The conductor handles this wrapping/unwrapping automatically.
    //!
    //! If you connected directly to an agent, your proxy would send `SuccessorMessage`
    //! envelopes that the agent doesn't understand.
    //!
    //! # Example: Complete proxy with conductor
    //!
    //! See the [`sacp-conductor` tests] for complete working examples of proxies
    //! running with the conductor.
    //!
    //! [`sacp-conductor`]: https://crates.io/crates/sacp-conductor
    //! [`Conductor`]: https://docs.rs/sacp-conductor/latest/sacp_conductor/struct.Conductor.html
    //! [`SuccessorMessage`]: crate::schema::SuccessorMessage
    //! [`sacp-conductor` tests]: https://github.com/symposium-dev/symposium-acp/tree/main/src/sacp-conductor/tests
}
