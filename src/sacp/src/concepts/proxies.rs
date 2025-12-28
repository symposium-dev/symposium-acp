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
//! - [`ClientPeer`] - messages from/to the client direction
//! - [`AgentPeer`] - messages from/to the agent direction
//!
//! Unlike simpler links, there's no default peer - you must always specify
//! which direction you're communicating with.
//!
//! # Default Forwarding
//!
//! By default, `ProxyToConductor` forwards all messages it doesn't handle.
//! This means a minimal proxy that does nothing is just:
//!
//! ```ignore
//! ProxyToConductor::builder()
//!     .serve(transport)
//!     .await?;
//! ```
//!
//! All messages pass through unchanged.
//!
//! # Intercepting Messages
//!
//! To intercept specific messages, use `on_receive_*_from` with explicit peers:
//!
//! ```ignore
//! ProxyToConductor::builder()
//!     // Intercept prompts from the client
//!     .on_receive_request_from(ClientPeer, async |req: PromptRequest, request_cx, cx| {
//!         // Modify the prompt
//!         let modified = PromptRequest {
//!             prompt: format!("Be helpful. {}", req.prompt),
//!             ..req
//!         };
//!
//!         // Forward to agent and relay the response back
//!         cx.send_request_to(AgentPeer, modified)
//!             .forward_to_request_cx(request_cx)
//!     }, on_receive_request!())
//!     .serve(transport)
//!     .await?;
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
//! ```ignore
//! ProxyToConductor::builder()
//!     .with_mcp_server(my_mcp_server)?
//!     .serve(transport)
//!     .await?;
//! ```
//!
//! ## Per-Session MCP Server
//!
//! ```ignore
//! ProxyToConductor::builder()
//!     .on_receive_request_from(ClientPeer, async |req: NewSessionRequest, request_cx, cx| {
//!         cx.build_session_from(req)?
//!             .with_mcp_server(my_mcp_server)?
//!             .on_proxy_session_start(request_cx, async |session_id| {
//!                 // Session started with MCP server attached
//!                 Ok(())
//!             })
//!     }, on_receive_request!())
//!     .serve(transport)
//!     .await?;
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
//! - `ClientPeer` is "toward the client" (Proxy A, or conductor if first)
//! - `AgentPeer` is "toward the agent" (Proxy B, or agent if last)
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
//! [`ClientPeer`]: crate::ClientPeer
//! [`AgentPeer`]: crate::AgentPeer
//! [`sacp-conductor`]: https://crates.io/crates/sacp-conductor
