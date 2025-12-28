#![deny(missing_docs)]

//! # sacp -- the Symposium Agent Client Protocol (ACP) SDK
//!
//! **sacp** is a Rust SDK for building [Agent-Client Protocol (ACP)][acp] applications.
//! ACP is a protocol for communication between AI agents and their clients (IDEs, CLIs, etc.),
//! enabling features like tool use, permission requests, and streaming responses.
//!
//! [acp]: https://agentclientprotocol.com/
//!
//! ## What can you build with sacp?
//!
//! - **Clients** that talk to ACP agents (like building your own Claude Code interface)
//! - **Proxies** that add capabilities to existing agents (like adding custom tools via MCP)
//! - **Agents** that respond to prompts with AI-powered responses
//!
//! ## Quick Start: Connecting to an Agent
//!
//! The most common use case is connecting to an existing ACP agent as a client.
//! Here's a minimal example that initializes a connection, creates a session,
//! and sends a prompt:
//!
//! ```no_run
//! use sacp::ClientToAgent;
//! use sacp::schema::{InitializeRequest, VERSION as PROTOCOL_VERSION};
//!
//! # async fn run(transport: impl sacp::Component<sacp::AgentToClient>) -> Result<(), sacp::Error> {
//! ClientToAgent::builder()
//!     .name("my-client")
//!     .run_until(transport, async |cx| {
//!         // Step 1: Initialize the connection
//!         cx.send_request(InitializeRequest {
//!             protocol_version: PROTOCOL_VERSION,
//!             client_capabilities: Default::default(),
//!             client_info: Default::default(),
//!             meta: None,
//!         }).block_task().await?;
//!
//!         // Step 2: Create a session and send a prompt
//!         cx.build_session_cwd()?
//!             .block_task()
//!             .run_until(async |mut session| {
//!                 session.send_prompt("What is 2 + 2?")?;
//!                 let response = session.read_to_string().await?;
//!                 println!("{}", response);
//!                 Ok(())
//!             })
//!             .await
//!     })
//!     .await
//! # }
//! ```
//!
//! For a complete working example, see [`yolo_one_shot_client.rs`][yolo].
//!
//! [yolo]: https://github.com/symposium-dev/symposium-acp/blob/main/src/sacp/examples/yolo_one_shot_client.rs
//!
//! ## Cookbook
//!
//! The [`cookbook`] module contains practical guides and examples:
//!
//! - [Connecting as a client](cookbook::connecting_as_client)
//! - [Global MCP server](cookbook::global_mcp_server)
//! - [Per-session MCP server](cookbook::per_session_mcp_server)
//! - [...and more!][`cookbook`]
//!
//! ## Core Concepts
//!
//! The [`concepts`] module provides detailed explanations of how sacp works,
//! including connections, sessions, callbacks, ordering guarantees, and more.
//!
//! ## Related Crates
//!
//! - [`sacp-tokio`] - Tokio utilities for spawning agent processes
//! - [`sacp-conductor`] - Binary for running proxy chains
//!
//! [`sacp-tokio`]: https://crates.io/crates/sacp-tokio
//! [`sacp-conductor`]: https://crates.io/crates/sacp-conductor

/// Capability management for the `_meta.symposium` object
mod capabilities;
/// Component abstraction for agents and proxies
pub mod component;
/// Core concepts for understanding and using sacp
pub mod concepts;
/// Cookbook of common patterns for building ACP components
pub mod cookbook;
/// JSON-RPC handler types for building custom message handlers
pub mod handler;
/// JSON-RPC connection and handler infrastructure
mod jsonrpc;
/// Link types for JSON-RPC connections
pub mod link;
/// MCP declarations (minimal)
pub mod mcp;
/// MCP server support for providing MCP tools over ACP
pub mod mcp_server;
/// Peer types for JSON-RPC connections
pub mod peer;
/// ACP protocol schema types - all message types, requests, responses, and supporting types
pub mod schema;
/// Utility functions and types
pub mod util;

pub use capabilities::*;

/// JSON-RPC message types.
///
/// This module re-exports types from the `jsonrpcmsg` crate that are transitively
/// reachable through the public API (e.g., via [`Channel`]).
///
/// Users of the `sacp` crate can use these types without adding a direct dependency
/// on `jsonrpcmsg`.
pub mod jsonrpcmsg {
    pub use jsonrpcmsg::{Error, Id, Message, Params, Request, Response};
}

pub use jsonrpc::{
    ByteStreams, Channel, Handled, IntoHandled, JrConnection, JrConnectionBuilder, JrConnectionCx,
    JrMessage, JrMessageHandler, JrNotification, JrRequest, JrRequestCx, JrResponse,
    JrResponsePayload, Lines, MessageCx, NullHandler, UntypedMessage,
    responder::{ChainResponder, JrResponder, NullResponder},
};

pub use link::{AgentToClient, ClientToAgent, HasDefaultPeer, HasPeer, JrLink, ProxyToConductor};

pub use peer::{AgentPeer, ClientPeer, ConductorPeer, JrPeer};

pub use component::{Component, DynComponent};

// Re-export BoxFuture for implementing Component traits
pub use futures::future::BoxFuture;

// Re-export the six primary message enum types at the root
pub use schema::{
    AgentNotification, AgentRequest, AgentResponse, ClientNotification, ClientRequest,
    ClientResponse,
};

// Re-export commonly used infrastructure types for convenience
pub use schema::{Error, ErrorCode};

// Re-export derive macros for custom JSON-RPC types
pub use sacp_derive::{JrNotification, JrRequest, JrResponsePayload};

mod session;
pub use session::*;

/// This is a hack that must be given as the final argument of
/// [`McpServerBuilder::tool_fn`](`crate::mcp_server::McpServerBuilder::tool_fn`) when defining tools.
/// Look away, lest ye be blinded by its vileness!
///
/// Fine, if you MUST know, it's a horrific workaround for not having
/// [return-type notation](https://github.com/rust-lang/rust/issues/109417)
/// and for [this !@$#!%! bug](https://github.com/rust-lang/rust/issues/110338).
/// Trust me, the need for it hurts me more than it hurts you. --nikomatsakis
#[macro_export]
macro_rules! tool_fn_mut {
    () => {
        |func, params, context| Box::pin(func(params, context))
    };
}

/// This is a hack that must be given as the final argument of
/// [`McpServerBuilder::tool_fn`](`crate::mcp_server::McpServerBuilder::tool_fn`) when defining stateless concurrent tools.
/// See [`tool_fn_mut!`] for the gory details.
#[macro_export]
macro_rules! tool_fn {
    () => {
        |func, params, context| Box::pin(func(params, context))
    };
}

/// This macro is used for the value of the `to_future_hack` parameter of
/// [`JrConnectionBuilder::on_receive_request`] and [`JrConnectionBuilder::on_receive_request_from`].
///
/// It expands to `|f, req, req_cx, cx| Box::pin(f(req, req_cx, cx))`.
///
/// This is needed until [return-type notation](https://github.com/rust-lang/rust/issues/109417)
/// is stabilized.
#[macro_export]
macro_rules! on_receive_request {
    () => {
        |f: &mut _, req, req_cx, cx| Box::pin(f(req, req_cx, cx))
    };
}

/// This macro is used for the value of the `to_future_hack` parameter of
/// [`JrConnectionBuilder::on_receive_notification`] and [`JrConnectionBuilder::on_receive_notification_from`].
///
/// It expands to `|f, notif, cx| Box::pin(f(notif, cx))`.
///
/// This is needed until [return-type notation](https://github.com/rust-lang/rust/issues/109417)
/// is stabilized.
#[macro_export]
macro_rules! on_receive_notification {
    () => {
        |f: &mut _, notif, cx| Box::pin(f(notif, cx))
    };
}

/// This macro is used for the value of the `to_future_hack` parameter of
/// [`JrConnectionBuilder::on_receive_message`] and [`JrConnectionBuilder::on_receive_message_from`].
///
/// It expands to `|f, msg_cx, cx| Box::pin(f(msg_cx, cx))`.
///
/// This is needed until [return-type notation](https://github.com/rust-lang/rust/issues/109417)
/// is stabilized.
#[macro_export]
macro_rules! on_receive_message {
    () => {
        |f: &mut _, msg_cx, cx| Box::pin(f(msg_cx, cx))
    };
}
