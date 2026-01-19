//! Building proxies that intercept and modify messages.
//!
//! A **proxy** sits between a client and an agent, intercepting messages
//! in both directions. This is how you add capabilities like MCP tools,
//! logging, or message transformation.
//!
//! # The Proxy Link Type
//!
//! Proxies use the [`ProxyToConductor`] link type, which has two peers:
//!
//! - [`Client`] - messages from/to the client direction
//! - [`Agent`] - messages from/to the agent direction
//!
//! Unlike simpler links, there's no default peer - you must always specify
//! which direction you're communicating with.
//!
//! # Default Forwarding
//!
//! By default, `ProxyToConductor` forwards all messages it doesn't handle.
//! This means a minimal proxy that does nothing is just:
//!
//! ```
//! # use sacp::{ProxyToConductor, Component};
//! # use sacp::link::ConductorToProxy;
//! # async fn example(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
//! Proxy.connect_from()
//!     .serve(transport)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! All messages pass through unchanged.
//!
//! # Intercepting Messages
//!
//! To intercept specific messages, use `on_receive_*_from` with explicit peers:
//!
//! ```
//! # use sacp::{ProxyToConductor, Client, Agent, Component};
//! # use sacp::link::ConductorToProxy;
//! # use sacp_test::ProcessRequest;
//! # async fn example(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
//! Proxy.connect_from()
//!     // Intercept requests from the client
//!     .on_receive_request_from(Client, async |req: ProcessRequest, request_cx, cx| {
//!         // Modify the request
//!         let modified = ProcessRequest {
//!             data: format!("prefix: {}", req.data),
//!         };
//!
//!         // Forward to agent and relay the response back
//!         cx.send_request_to(Agent, modified)
//!             .forward_to_request_cx(request_cx)
//!     }, sacp::on_receive_request!())
//!     .serve(transport)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! Messages you don't handle are forwarded automatically.
//!
//! # Adding MCP Servers
//!
//! A common use case is adding tools via MCP. You can add them globally
//! (available in all sessions) or per-session.
//!
//! ## Global MCP Server
//!
//! ```
//! # use sacp::{ProxyToConductor, Component};
//! # use sacp::link::ConductorToProxy;
//! # use sacp::mcp_server::McpServer;
//! # async fn example(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
//! # let my_mcp_server = McpServer::<ProxyToConductor, _>::builder("tools").build();
//! Proxy.connect_from()
//!     .with_mcp_server(my_mcp_server)
//!     .serve(transport)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Per-Session MCP Server
//!
//! ```
//! # use sacp::{ProxyToConductor, Client, Component};
//! # use sacp::link::ConductorToProxy;
//! # use sacp::schema::NewSessionRequest;
//! # use sacp::mcp_server::McpServer;
//! # async fn example(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
//! Proxy.connect_from()
//!     .on_receive_request_from(Client, async |req: NewSessionRequest, request_cx, cx| {
//!         let my_mcp_server = McpServer::<ProxyToConductor, _>::builder("tools").build();
//!         cx.build_session_from(req)
//!             .with_mcp_server(my_mcp_server)?
//!             .on_proxy_session_start(request_cx, async |session_id| {
//!                 // Session started with MCP server attached
//!                 Ok(())
//!             })
//!     }, sacp::on_receive_request!())
//!     .serve(transport)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! # The Conductor
//!
//! Proxies don't run standalone - they're orchestrated by a **conductor**.
//! The conductor:
//!
//! - Spawns proxy processes
//! - Chains them together
//! - Connects the final proxy to the agent
//!
//! The [`sacp-conductor`] crate provides a conductor binary. You configure
//! it with a list of proxies to run.
//!
//! # Proxy Chains
//!
//! Multiple proxies can be chained:
//!
//! ```text
//! Client <-> Proxy A <-> Proxy B <-> Agent
//! ```
//!
//! Each proxy sees messages from its perspective:
//! - `Client` is "toward the client" (Proxy A, or conductor if first)
//! - `Agent` is "toward the agent" (Proxy B, or agent if last)
//!
//! Messages flow through each proxy in order. Each can inspect, modify,
//! or handle messages before they continue.
//!
//! # Summary
//!
//! | Task | Approach |
//! |------|----------|
//! | Forward everything | Just `serve(transport)` |
//! | Intercept specific messages | `on_receive_*_from` with explicit peers |
//! | Add global tools | `with_mcp_server` on builder |
//! | Add per-session tools | `with_mcp_server` on session builder |
//!
//! [`ProxyToConductor`]: crate::ProxyToConductor
//! [`Client`]: crate::Client
//! [`Agent`]: crate::Agent
//! [`sacp-conductor`]: https://crates.io/crates/sacp-conductor
