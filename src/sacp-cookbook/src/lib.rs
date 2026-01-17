//! Cookbook of common patterns for building ACP components.
//!
//! This crate contains guides and examples for the three main things you can build with sacp:
//!
//! - **Clients** - Connect to an existing agent and send prompts
//! - **Proxies** - Sit between client and agent to add capabilities (like MCP tools)
//! - **Agents** - Respond to prompts with AI-powered responses
//!
//! See the [`sacp::concepts`] module for detailed explanations of
//! the concepts behind the API.
//!
//! # Building Clients
//!
//! A client connects to an agent, sends requests, and handles responses. Use
//! [`ClientToAgent`] as your link type.
//!
//! - [`one_shot_prompt`] - Send a single prompt and get a response (simplest pattern)
//! - [`connecting_as_client`] - More details on connection setup and permission handling
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
//! - [`filtering_tools`] - Enable or disable tools dynamically
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
//! [`sacp::concepts`]: sacp::concepts
//! [`ClientToAgent`]: sacp::ClientToAgent
//! [`AgentToClient`]: sacp::AgentToClient
//! [`ProxyToConductor`]: sacp::ProxyToConductor
//! [`Component`]: sacp::Component

pub mod one_shot_prompt {
    //! Pattern: You Only Prompt Once.
    //!
    //! The simplest client pattern: connect to an agent, send one prompt, get the
    //! response. This is useful for CLI tools, scripts, or any case where you just
    //! need a single interaction with an agent.
    //!
    //! # Example
    //!
    //! ```
    //! use sacp::{ClientToAgent, AgentToClient, Component};
    //! use sacp::schema::{InitializeRequest, ProtocolVersion};
    //!
    //! async fn ask_agent(
    //!     transport: impl Component<AgentToClient> + 'static,
    //!     prompt: &str,
    //! ) -> Result<String, sacp::Error> {
    //!     ClientToAgent::builder()
    //!         .name("my-client")
    //!         .connect_to(transport)?
    //!         .run_until(async |cx| {
    //!             // Initialize the connection
    //!             cx.send_request(InitializeRequest::new(ProtocolVersion::LATEST))
    //!                 .block_task().await?;
    //!
    //!             // Create a session, send prompt, read response
    //!             let mut session = cx.build_session_cwd()?
    //!                 .block_task()
    //!                 .start_session()
    //!                 .await?;
    //!
    //!             session.send_prompt(prompt)?;
    //!             session.read_to_string().await
    //!         })
    //!         .await
    //! }
    //! ```
    //!
    //! # How it works
    //!
    //! 1. **[`connect_to`]** establishes the transport connection
    //! 2. **[`run_until`]** runs your code while the connection handles messages
    //!    in the background
    //! 3. **[`send_request`]** + **[`block_task`]** sends the initialize request
    //!    and waits for the response
    //! 4. **[`build_session_cwd`]** creates a session builder using the current working directory
    //! 5. **[`start_session`]** sends the `NewSessionRequest` and returns an
    //!    [`ActiveSession`] handle
    //! 6. **[`send_prompt`]** queues the prompt to send to the agent
    //! 7. **[`read_to_string`]** reads all text chunks until the agent finishes
    //!
    //! # Handling permission requests
    //!
    //! Most agents will ask for permission before taking actions like running
    //! commands or writing files. See [`connecting_as_client`] for how to handle
    //! [`RequestPermissionRequest`] messages.
    //!
    //! [`connect_to`]: sacp::ConnectFrom::connect_to
    //! [`run_until`]: sacp::ConnectFrom::run_until
    //! [`send_request`]: sacp::ConnectionTo::send_request
    //! [`block_task`]: sacp::JrResponse::block_task
    //! [`build_session_cwd`]: sacp::ConnectionTo::build_session_cwd
    //! [`start_session`]: sacp::SessionBuilder::start_session
    //! [`ActiveSession`]: sacp::ActiveSession
    //! [`send_prompt`]: sacp::ActiveSession::send_prompt
    //! [`read_to_string`]: sacp::ActiveSession::read_to_string
    //! [`connecting_as_client`]: super::connecting_as_client
    //! [`RequestPermissionRequest`]: sacp::schema::RequestPermissionRequest
}

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
    //! use sacp::schema::{InitializeRequest, ProtocolVersion};
    //!
    //! async fn connect_to_agent(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
    //!     ClientToAgent::builder()
    //!         .name("my-client")
    //!         .run_until(transport, async |cx| {
    //!             // Initialize the connection
    //!             cx.send_request(InitializeRequest::new(ProtocolVersion::LATEST))
    //!                 .block_task().await?;
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
    //! [`run_until`]: sacp::ConnectFrom::run_until
    //! [`block_task`]: sacp::JrResponse::block_task
    //! [`build_session`]: sacp::ConnectionTo::build_session
    //! [`SessionBuilder`]: sacp::SessionBuilder
    //! [`send_prompt`]: sacp::ActiveSession::send_prompt
    //! [`read_update`]: sacp::ActiveSession::read_update
    //! [`read_to_string`]: sacp::ActiveSession::read_to_string
    //! [`with_mcp_server`]: sacp::SessionBuilder::with_mcp_server
    //! [`RequestPermissionRequest`]: sacp::schema::RequestPermissionRequest
    //! [`on_receive_request`]: sacp::ConnectFrom::on_receive_request
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
    //! use sacp::{AgentToClient, Component, MessageCx, ConnectionTo};
    //! use sacp::link::JrLink;
    //! use sacp::schema::{
    //!     InitializeRequest, InitializeResponse, AgentCapabilities,
    //!     NewSessionRequest, NewSessionResponse, SessionId,
    //!     PromptRequest, PromptResponse, StopReason,
    //! };
    //!
    //! async fn run_agent(transport: impl Component<sacp::ClientToAgent>) -> Result<(), sacp::Error> {
    //!     AgentToClient::builder()
    //!         .name("my-agent")
    //!         // Handle initialization
    //!         .on_receive_request(async |req: InitializeRequest, request_cx, _cx| {
    //!             request_cx.respond(
    //!                 InitializeResponse::new(req.protocol_version)
    //!                     .agent_capabilities(AgentCapabilities::new())
    //!             )
    //!         }, sacp::on_receive_request!())
    //!         // Handle session creation
    //!         .on_receive_request(async |req: NewSessionRequest, request_cx, _cx| {
    //!             request_cx.respond(NewSessionResponse::new(SessionId::new("session-1")))
    //!         }, sacp::on_receive_request!())
    //!         // Handle prompts
    //!         .on_receive_request(async |req: PromptRequest, request_cx, cx| {
    //!             // Send streaming updates via notifications
    //!             // cx.send_notification(SessionNotification { ... })?;
    //!
    //!             // Return final response
    //!             request_cx.respond(PromptResponse::new(StopReason::EndTurn))
    //!         }, sacp::on_receive_request!())
    //!         // Reject unknown messages
    //!         .on_receive_message(async |message: MessageCx, cx: ConnectionTo<AgentToClient>| {
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
    //! [`InitializeRequest`]: sacp::schema::InitializeRequest
    //! [`NewSessionRequest`]: sacp::schema::NewSessionRequest
    //! [`PromptRequest`]: sacp::schema::PromptRequest
    //! [`SessionNotification`]: sacp::schema::SessionNotification
    //! [`RequestPermissionRequest`]: sacp::schema::RequestPermissionRequest
    //! [`AgentToClient`]: sacp::AgentToClient
    //! [`Component`]: sacp::Component
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
    //!                 request_cx.respond(
    //!                     InitializeResponse::new(req.protocol_version)
    //!                         .agent_capabilities(AgentCapabilities::new())
    //!                 )
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
    //! - Use [`ConnectionTo::spawn`] to offload work to a background task
    //! - Use [`on_receiving_result`] to schedule work when a response arrives
    //!
    //! [`Component`]: sacp::Component
    //! [`ConnectionTo::spawn`]: sacp::ConnectionTo::spawn
    //! [`on_receiving_result`]: sacp::JrResponse::on_receiving_result
    //! [`sacp-conductor`]: https://crates.io/crates/sacp-conductor
}

pub mod custom_message_handlers {
    //! Pattern: Custom message handlers.
    //!
    //! For reusable message handling logic, implement [`HandleMessageFrom`] and use
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
    //! use sacp::{HandleMessageFrom, MessageCx, Handled, ConnectionTo};
    //! use sacp::schema::{InitializeRequest, InitializeResponse, AgentCapabilities};
    //! use sacp::util::MatchMessage;
    //!
    //! struct MyHandler;
    //!
    //! impl HandleMessageFrom for MyHandler {
    //!     type Link = sacp::link::UntypedLink;
    //!
    //!     async fn handle_message(
    //!         &mut self,
    //!         message: MessageCx,
    //!         _cx: ConnectionTo<Self::Link>,
    //!     ) -> Result<Handled<MessageCx>, sacp::Error> {
    //!         MatchMessage::new(message)
    //!             .if_request(async |req: InitializeRequest, request_cx| {
    //!                 request_cx.respond(
    //!                     InitializeResponse::new(req.protocol_version)
    //!                         .agent_capabilities(AgentCapabilities::new())
    //!                 )
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
    //! [`HandleMessageFrom`]: sacp::HandleMessageFrom
    //! [`MatchMessage`]: sacp::util::MatchMessage
    //! [`MatchMessageFrom`]: sacp::util::MatchMessageFrom
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
    //! # Using the builder API
    //!
    //! The simplest way to create an MCP server is with [`McpServer::builder`]:
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::{Component, Run, ProxyToConductor};
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
    //! impl<R: Run<ProxyToConductor> + Send + 'static> Component<ProxyToConductor> for MyProxy<R> {
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
    //! # Using rmcp
    //!
    //! If you have an existing [rmcp](https://docs.rs/rmcp) server implementation,
    //! use [`McpServer::from_rmcp`] from the `sacp-rmcp` crate:
    //!
    //! ```
    //! use rmcp::{ServerHandler, tool, tool_router, tool_handler};
    //! use rmcp::handler::server::router::tool::ToolRouter;
    //! use rmcp::handler::server::wrapper::Parameters;
    //! use rmcp::model::*;
    //! use sacp::mcp_server::McpServer;
    //! use sacp::ProxyToConductor;
    //! use sacp_rmcp::McpServerExt;
    //! use serde::{Deserialize, Serialize};
    //!
    //! #[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
    //! struct EchoParams {
    //!     message: String,
    //! }
    //!
    //! #[derive(Clone)]
    //! struct MyMcpServer {
    //!     tool_router: ToolRouter<Self>,
    //! }
    //!
    //! impl MyMcpServer {
    //!     fn new() -> Self {
    //!         Self { tool_router: Self::tool_router() }
    //!     }
    //! }
    //!
    //! #[tool_router]
    //! impl MyMcpServer {
    //!     #[tool(description = "Echoes back the input message")]
    //!     async fn echo(&self, Parameters(params): Parameters<EchoParams>) -> Result<CallToolResult, rmcp::ErrorData> {
    //!         Ok(CallToolResult::success(vec![Content::text(format!("Echo: {}", params.message))]))
    //!     }
    //! }
    //!
    //! #[tool_handler]
    //! impl ServerHandler for MyMcpServer {
    //!     fn get_info(&self) -> ServerInfo {
    //!         ServerInfo {
    //!             protocol_version: ProtocolVersion::V_2024_11_05,
    //!             capabilities: ServerCapabilities::builder().enable_tools().build(),
    //!             server_info: Implementation::from_build_env(),
    //!             instructions: None,
    //!         }
    //!     }
    //! }
    //!
    //! // Create an MCP server from the rmcp service
    //! let mcp_server = McpServer::<ProxyToConductor, _>::from_rmcp("my-server", MyMcpServer::new);
    //! ```
    //!
    //! The `from_rmcp` function takes a factory closure that creates a new server
    //! instance. This allows each MCP connection to get a fresh server instance.
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
    //! [`McpServer::builder`]: sacp::mcp_server::McpServer::builder
    //! [`McpServer::from_rmcp`]: sacp_rmcp::McpServerExt::from_rmcp
    //! [`with_mcp_server`]: sacp::ConnectFrom::with_mcp_server
}

pub mod per_session_mcp_server {
    //! Pattern: Per-session MCP server with workspace context.
    //!
    //! Use this pattern when each session needs its own MCP server instance
    //! with access to session-specific context like the working directory.
    //!
    //! # When to use
    //!
    //! - Tools need access to the session's working directory
    //! - You want to track active sessions or maintain per-session state
    //! - Tools need to customize behavior based on session parameters
    //!
    //! # Basic pattern with `on_proxy_session_start`
    //!
    //! The most common pattern intercepts [`NewSessionRequest`], extracts context,
    //! creates a per-session MCP server, and uses [`on_proxy_session_start`] to
    //! run code after the session is established:
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::schema::NewSessionRequest;
    //! use sacp::{ClientPeer, Component, ProxyToConductor};
    //! use sacp::link::ConductorToProxy;
    //!
    //! async fn run_proxy(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
    //!     ProxyToConductor::builder()
    //!         .on_receive_request_from(ClientPeer, async move |request: NewSessionRequest, request_cx, cx| {
    //!             // Extract session context from the request
    //!             let workspace_path = request.cwd.clone();
    //!
    //!             // Create tools that capture the workspace path
    //!             let mcp_server = McpServer::builder("workspace-tools")
    //!                 .tool_fn("get_workspace", "Returns the session's workspace directory", {
    //!                     async move |_params: (), _cx| {
    //!                         Ok(workspace_path.display().to_string())
    //!                     }
    //!                 }, sacp::tool_fn!())
    //!                 .build();
    //!
    //!             // Build the session and run code after it starts
    //!             cx.build_session_from(request)
    //!                 .with_mcp_server(mcp_server)?
    //!                 .on_proxy_session_start(request_cx, async move |session_id| {
    //!                     // This callback runs after the session-id has been sent to the
    //!                     // client but before any further messages from the client or agent
    //!                     // related to this session have been processed.
    //!                     //
    //!                     // You can use this to store the `session_id` before processing
    //!                     // future messages, or to send a first prompt to the agent before
    //!                     // the client has a chance to do so.
    //!                     tracing::info!(%session_id, "Session started");
    //!                     Ok(())
    //!                 })
    //!         }, sacp::on_receive_request!())
    //!         .serve(transport)
    //!         .await
    //! }
    //! ```
    //!
    //! # How `on_proxy_session_start` works
    //!
    //! [`on_proxy_session_start`] is the non-blocking way to set up a proxy session:
    //!
    //! 1. Sends `NewSessionRequest` to the agent
    //! 2. When the response arrives, responds to the client automatically
    //! 3. Sets up message proxying for the session
    //! 4. Runs your callback with the `SessionId`
    //!
    //! The callback runs after the session is established but doesn't block
    //! the message handler. This is ideal for proxies that just need to inject
    //! tools and track sessions.
    //!
    //! # Alternative: blocking with `start_session_proxy`
    //!
    //! If you need the simpler blocking API (e.g., in a client context where
    //! blocking is safe), use [`block_task`] + [`start_session_proxy`]:
    //!
    //! ```
    //! # use sacp::mcp_server::McpServer;
    //! # use sacp::schema::NewSessionRequest;
    //! # use sacp::{ClientPeer, Component, ProxyToConductor};
    //! # use sacp::link::ConductorToProxy;
    //! # async fn run_proxy(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
    //!     ProxyToConductor::builder()
    //!         .on_receive_request_from(ClientPeer, async |request: NewSessionRequest, request_cx, cx| {
    //!             let cwd = request.cwd.clone();
    //!             let mcp_server = McpServer::builder("tools")
    //!                 .tool_fn("get_cwd", "Returns working directory", {
    //!                     async move |_params: (), _cx| Ok(cwd.display().to_string())
    //!                 }, sacp::tool_fn!())
    //!                 .build();
    //!
    //!             let session_id = cx.build_session_from(request)
    //!                 .with_mcp_server(mcp_server)?
    //!                 .block_task()
    //!                 .start_session_proxy(request_cx)
    //!                 .await?;
    //!
    //!             tracing::info!(%session_id, "Session started");
    //!             Ok(())
    //!         }, sacp::on_receive_request!())
    //!         .serve(transport)
    //!         .await
    //! # }
    //! ```
    //!
    //! For patterns where you need to interact with the session before proxying,
    //! use [`start_session`] + [`proxy_remaining_messages`] instead.
    //!
    //! [`start_session`]: sacp::SessionBuilder::start_session
    //! [`proxy_remaining_messages`]: sacp::ActiveSession::proxy_remaining_messages
    //!
    //! [`NewSessionRequest`]: sacp::schema::NewSessionRequest
    //! [`on_proxy_session_start`]: sacp::SessionBuilder::on_proxy_session_start
    //! [`block_task`]: sacp::SessionBuilder::block_task
    //! [`start_session_proxy`]: sacp::SessionBuilder::start_session_proxy
}

pub mod filtering_tools {
    //! Pattern: Filtering which tools are available.
    //!
    //! Use [`disable_tool`] and [`enable_tool`] to control which tools are
    //! visible to clients. This is useful when:
    //!
    //! - Some tools should only be available in certain configurations
    //! - You want to conditionally expose tools based on runtime settings
    //! - You need to restrict access to sensitive tools
    //!
    //! # Disabling specific tools (deny-list)
    //!
    //! By default, all registered tools are enabled. Use [`disable_tool`] to
    //! hide specific tools:
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::ProxyToConductor;
    //! use schemars::JsonSchema;
    //! use serde::Deserialize;
    //!
    //! #[derive(Debug, Deserialize, JsonSchema)]
    //! struct Params {}
    //!
    //! fn build_server(enable_admin: bool) -> Result<McpServer<ProxyToConductor, impl sacp::Run<ProxyToConductor>>, sacp::Error> {
    //!     let mut builder = McpServer::builder("my-server")
    //!         .tool_fn("echo", "Echo a message",
    //!             async |_p: Params, _cx| Ok("echoed"),
    //!             sacp::tool_fn!())
    //!         .tool_fn("admin", "Admin-only tool",
    //!             async |_p: Params, _cx| Ok("admin action"),
    //!             sacp::tool_fn!());
    //!
    //!     // Conditionally disable the admin tool
    //!     if !enable_admin {
    //!         builder = builder.disable_tool("admin")?;
    //!     }
    //!
    //!     Ok(builder.build())
    //! }
    //! ```
    //!
    //! Disabled tools:
    //! - Don't appear in `list_tools` responses
    //! - Return "tool not found" errors if called directly
    //!
    //! # Enabling only specific tools (allow-list)
    //!
    //! Use [`disable_all_tools`] followed by [`enable_tool`] to create an
    //! allow-list where only explicitly enabled tools are available:
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::ProxyToConductor;
    //! use schemars::JsonSchema;
    //! use serde::Deserialize;
    //!
    //! #[derive(Debug, Deserialize, JsonSchema)]
    //! struct Params {}
    //!
    //! fn build_restricted_server() -> Result<McpServer<ProxyToConductor, impl sacp::Run<ProxyToConductor>>, sacp::Error> {
    //!     McpServer::builder("restricted-server")
    //!         .tool_fn("safe", "Safe operation",
    //!             async |_p: Params, _cx| Ok("safe"),
    //!             sacp::tool_fn!())
    //!         .tool_fn("dangerous", "Dangerous operation",
    //!             async |_p: Params, _cx| Ok("danger!"),
    //!             sacp::tool_fn!())
    //!         .tool_fn("experimental", "Experimental feature",
    //!             async |_p: Params, _cx| Ok("experimental"),
    //!             sacp::tool_fn!())
    //!         // Start with all tools disabled
    //!         .disable_all_tools()
    //!         // Only enable the safe tool
    //!         .enable_tool("safe")
    //!         .map(|b| b.build())
    //! }
    //! ```
    //!
    //! # Error handling
    //!
    //! Both [`enable_tool`] and [`disable_tool`] return `Result` and will error
    //! if the tool name doesn't match any registered tool. This helps catch typos:
    //!
    //! ```
    //! use sacp::mcp_server::McpServer;
    //! use sacp::ProxyToConductor;
    //!
    //! // This will error because "ech" is not a registered tool
    //! let result = McpServer::<ProxyToConductor, _>::builder("server")
    //!     .disable_tool("ech");  // Typo! Should be "echo"
    //!
    //! assert!(result.is_err());
    //! ```
    //!
    //! Calling enable/disable on an already enabled/disabled tool is not an error -
    //! the operations are idempotent.
    //!
    //! [`disable_tool`]: sacp::mcp_server::McpServerBuilder::disable_tool
    //! [`enable_tool`]: sacp::mcp_server::McpServerBuilder::enable_tool
    //! [`disable_all_tools`]: sacp::mcp_server::McpServerBuilder::disable_all_tools
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
    //! [`Conductor`]: sacp_conductor::Conductor
    //! [`SuccessorMessage`]: sacp::schema::SuccessorMessage
    //! [`sacp-conductor` tests]: https://github.com/symposium-dev/symposium-acp/tree/main/src/sacp-conductor/tests
}
