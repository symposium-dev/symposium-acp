//! Cookbook of common patterns for building ACP components.
//!
//! This module contains documented examples of patterns that come up
//! frequently when building agents, proxies, and other ACP components.
//!
//! # Roles and Endpoints
//!
//! ACP connections are typed by their *role*, which captures both "who I am"
//! and "who I'm talking to". Roles implement [`JrRole`] and determine what
//! operations are valid on a connection.
//!
//! ## Endpoints
//!
//! *Endpoints* ([`JrEndpoint`]) are logical destinations for messages:
//!
//! - [`Client`] - The client endpoint (IDE, CLI, etc.)
//! - [`Agent`] - The agent endpoint (AI-powered component)
//! - [`Conductor`] - The conductor endpoint (orchestrates proxy chains)
//!
//! Most roles have a single implicit endpoint, but proxies can send to
//! multiple endpoints. Use [`send_request_to`] and [`send_notification_to`]
//! to specify the destination explicitly.
//!
//! ## Role Types
//!
//! The built-in role types are:
//!
//! | Role | Description | Can send to |
//! |------|-------------|-------------|
//! | [`ClientToAgent`] | Client's connection to an agent | `Agent` |
//! | [`AgentToClient`] | Agent's connection to a client | `Client` |
//! | [`ProxyToConductor`] | Proxy's connection to the conductor | `Client`, `Agent` |
//! | [`ConductorToClient`] | Conductor's connection to a client | `Client`, `Agent` |
//! | [`ConductorToProxy`] | Conductor's connection to a proxy | `Agent` |
//! | [`ConductorToAgent`] | Conductor's connection to the final agent | `Agent` |
//! | [`UntypedRole`] | Generic role for testing/dynamic scenarios | any |
//!
//! ## Proxies and Multiple Endpoints
//!
//! A proxy sits between client and agent, so it needs to send messages in
//! both directions. [`ProxyToConductor`] implements `HasEndpoint<Client>` and
//! `HasEndpoint<Agent>`, allowing it to forward messages appropriately:
//!
//! ```ignore
//! // Forward a request toward the agent
//! cx.send_request_to(Agent, request).forward_to_request_cx(request_cx)?;
//!
//! // Send a notification toward the client
//! cx.send_notification_to(Client, notification)?;
//! ```
//!
//! When sending to `Agent` from a proxy, messages are automatically wrapped
//! in [`SuccessorMessage`] envelopes. When receiving from `Agent`, they're
//! automatically unwrapped.
//!
//! [`JrRole`]: crate::role::JrRole
//! [`JrEndpoint`]: crate::role::JrEndpoint
//! [`Client`]: crate::Client
//! [`Agent`]: crate::Agent
//! [`Conductor`]: crate::Conductor
//! [`ClientToAgent`]: crate::ClientToAgent
//! [`AgentToClient`]: crate::AgentToClient
//! [`ProxyToConductor`]: crate::ProxyToConductor
//! [`ConductorToClient`]: crate::role::ConductorToClient
//! [`ConductorToProxy`]: crate::role::ConductorToProxy
//! [`ConductorToAgent`]: crate::role::ConductorToAgent
//! [`UntypedRole`]: crate::role::UntypedRole
//! [`SuccessorMessage`]: crate::schema::SuccessorMessage
//! [`send_request_to`]: crate::JrConnectionCx::send_request_to
//! [`send_notification_to`]: crate::JrConnectionCx::send_notification_to
//!
//! # Patterns
//!
//! - [`reusable_components`] - Defining agents/proxies with [`Component`]
//! - [`custom_message_handlers`] - Implementing [`JrMessageHandler`]
//! - [`connecting_as_client`] - Using `with_client` to send requests
//! - [`global_mcp_server`] - Adding a shared MCP server to a handler chain
//! - [`per_session_mcp_server`] - Creating per-session MCP servers
//!
//! [`Component`]: crate::Component
//! [`JrMessageHandler`]: crate::JrMessageHandler
//! [`reusable_components`]: crate::cookbook::reusable_components
//! [`custom_message_handlers`]: crate::cookbook::custom_message_handlers
//! [`connecting_as_client`]: crate::cookbook::connecting_as_client
//! [`global_mcp_server`]: crate::cookbook::global_mcp_server
//! [`per_session_mcp_server`]: crate::cookbook::per_session_mcp_server

pub mod reusable_components {
    //! Pattern: Defining reusable components.
    //!
    //! When building agents or proxies, define a struct that implements [`Component`].
    //! Internally, use the role's `builder()` method to set up handlers.
    //!
    //! # Example
    //!
    //! ```
    //! use sacp::{Component, AgentToClient};
    //! use sacp::schema::{
    //!     InitializeRequest, InitializeResponse, AgentCapabilities,
    //! };
    //!
    //! struct MyAgent {
    //!     name: String,
    //! }
    //!
    //! impl Component for MyAgent {
    //!     async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
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
}

pub mod custom_message_handlers {
    //! Pattern: Custom message handlers.
    //!
    //! For reusable message handling logic, implement [`JrMessageHandler`] and use
    //! [`MatchMessage`] or [`MatchMessageFrom`] for type-safe dispatching.
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
    //!     type Role = sacp::role::UntypedRole;
    //!
    //!     async fn handle_message(
    //!         &mut self,
    //!         message: MessageCx,
    //!         _cx: JrConnectionCx<Self::Role>,
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
    //! - [`MatchMessage`] - Use when you don't need endpoint-aware handling
    //! - [`MatchMessageFrom`] - Use in proxies where messages come from different
    //!   endpoints (`Client` vs `Agent`) and may need different handling
    //!
    //! [`JrMessageHandler`]: crate::JrMessageHandler
    //! [`MatchMessage`]: crate::util::MatchMessage
    //! [`MatchMessageFrom`]: crate::util::MatchMessageFrom
}

pub mod connecting_as_client {
    //! Pattern: Connecting as a client.
    //!
    //! To connect to a JSON-RPC server and send requests, use [`with_client`].
    //! This gives you a connection context for sending requests while the
    //! connection handles incoming messages in the background.
    //!
    //! # Example
    //!
    //! ```
    //! use sacp::{ClientToAgent, Component};
    //! use sacp::schema::{InitializeRequest, NewSessionRequest, SessionNotification};
    //!
    //! async fn connect_to_agent(transport: impl Component) -> Result<(), sacp::Error> {
    //!     ClientToAgent::builder()
    //!         .name("my-client")
    //!         .on_receive_notification(async |notif: SessionNotification, _cx| {
    //!             // Handle notifications from the agent
    //!             println!("Session updated: {:?}", notif);
    //!             Ok(())
    //!         }, sacp::on_receive_notification!())
    //!         .with_client(transport, async |cx| {
    //!             // Initialize the connection
    //!             let _init_response = cx.send_request(InitializeRequest {
    //!                 protocol_version: Default::default(),
    //!                 client_capabilities: Default::default(),
    //!                 client_info: None,
    //!                 meta: None,
    //!             })
    //!                 .block_task()
    //!                 .await?;
    //!
    //!             // Create a session
    //!             let session = cx.send_request(NewSessionRequest {
    //!                 cwd: ".".into(),
    //!                 mcp_servers: vec![],
    //!                 meta: None,
    //!             })
    //!                 .block_task()
    //!                 .await?;
    //!
    //!             println!("Session created: {:?}", session.session_id);
    //!             Ok(())
    //!         })
    //!         .await
    //! }
    //! ```
    //!
    //! # Note on `block_task`
    //!
    //! Using [`block_task`] is safe inside `with_client` because the closure runs
    //! as a spawned task, not on the event loop. The event loop continues processing
    //! messages (including the response you're waiting for) while your task blocks.
    //!
    //! [`with_client`]: crate::JrConnectionBuilder::with_client
    //! [`block_task`]: crate::JrResponse::block_task
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
    //! impl<R: JrResponder<ProxyToConductor> + Send + 'static> Component for MyProxy<R> {
    //!     async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
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
    //! # Example
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::schema::NewSessionRequest;
    //! use sacp::{Agent, Client, Component, JrResponder, ProxyToConductor};
    //!
    //! async fn run_proxy(transport: impl Component) -> Result<(), sacp::Error> {
    //!     ProxyToConductor::builder()
    //!         .on_receive_request_from(Client, async |request: NewSessionRequest, request_cx, cx| {
    //!             // Create an MCP server for this session
    //!             let cwd = request.cwd.clone();
    //!             let mcp_server = McpServer::builder("session-tools")
    //!                 .tool_fn("get_cwd", "Returns session working directory",
    //!                     async move |_params: (), _cx| {
    //!                         Ok(cwd.display().to_string())
    //!                     }, sacp::tool_fn!())
    //!                 .build();
    //!
    //!             // Build the session with the MCP server attached and proxy it
    //!             cx.build_session_from(request)
    //!                 .with_mcp_server(mcp_server)?
    //!                 .proxy_session(request_cx, JrResponder::run)
    //!                 .await
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
    //! 4. The handler lives as long as the session (dropped when `run_session` completes)
    //!
    //! [`SessionBuilder::with_mcp_server`]: crate::SessionBuilder::with_mcp_server
}
