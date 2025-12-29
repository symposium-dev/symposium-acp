//! Establishing connections using link types and connection builders.
//!
//! To communicate over ACP, you need to establish a connection. This involves
//! choosing a **link type** that matches your role and using a **connection builder**
//! to configure and run the connection.
//!
//! # Choosing a Link Type
//!
//! Your link type determines what messages you can send and who you can send them to.
//! Choose based on what you're building:
//!
//! | You are building... | Use this link type |
//! |---------------------|-------------------|
//! | A client that talks to an agent | [`ClientToAgent`] |
//! | An agent that responds to clients | [`AgentToClient`] |
//! | A proxy in a conductor chain | [`ProxyToConductor`] |
//!
//! # The Connection Builder Pattern
//!
//! Every link type has a `builder()` method that returns a connection builder.
//! The builder lets you configure handlers, then connect to a transport:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, Component};
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! ClientToAgent::builder()
//!     .name("my-client")
//!     .run_until(transport, async |cx| {
//!         // Use `cx` to send requests and handle responses
//!         Ok(())
//!     })
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! # The Connection Context
//!
//! Inside `run_until`, you receive a [`JrConnectionCx`] (connection context) that
//! lets you interact with the remote peer:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, Component};
//! # use sacp::schema::InitializeRequest;
//! # use sacp_test::StatusUpdate;
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! # ClientToAgent::builder().run_until(transport, async |cx| {
//! // Send a request and wait for the response
//! let response = cx.send_request(InitializeRequest {
//!     protocol_version: Default::default(),
//!     client_capabilities: Default::default(),
//!     client_info: None,
//!     meta: None,
//! })
//!     .block_task()
//!     .await?;
//!
//! // Send a notification (fire-and-forget)
//! cx.send_notification(StatusUpdate { message: "hello".into() })?;
//! # Ok(())
//! # }).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Sending Requests
//!
//! When you call `send_request()`, you get back a [`JrResponse`] that represents
//! the pending response. You have two main ways to handle it:
//!
//! ## Option 1: Block and wait
//!
//! Use `block_task()` when you need the response before continuing:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, Component};
//! # use sacp_test::MyRequest;
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! # ClientToAgent::builder().run_until(transport, async |cx| {
//! let response = cx.send_request(MyRequest {})
//!     .block_task()
//!     .await?;
//! // Use response here
//! # Ok(())
//! # }).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Option 2: Schedule a callback
//!
//! Use `on_receiving_result()` when you want to handle the response asynchronously:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, Component};
//! # use sacp_test::MyRequest;
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! # ClientToAgent::builder().run_until(transport, async |cx| {
//! cx.send_request(MyRequest {})
//!     .on_receiving_result(async |result| {
//!         match result {
//!             Ok(response) => { /* handle success */ }
//!             Err(error) => { /* handle error */ }
//!         }
//!         Ok(())
//!     })?;
//! // Continues immediately, callback runs when response arrives
//! # Ok(())
//! # }).await?;
//! # Ok(())
//! # }
//! ```
//!
//! See [Ordering](super::ordering) for important details about how these differ.
//!
//! # Next Steps
//!
//! - [Sessions](super::sessions) - Create multi-turn conversations
//! - [Callbacks](super::callbacks) - Handle incoming requests from the remote peer
//!
//! [`ClientToAgent`]: crate::ClientToAgent
//! [`AgentToClient`]: crate::AgentToClient
//! [`ProxyToConductor`]: crate::ProxyToConductor
//! [`JrConnectionCx`]: crate::JrConnectionCx
//! [`JrResponse`]: crate::JrResponse
