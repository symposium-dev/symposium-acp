//! Explicit peers: using `_to` and `_from` variants.
//!
//! So far, we've used methods like `send_request` and `on_receive_request`
//! without specifying *who* we're sending to or receiving from. That's because
//! each link type has a **default peer**.
//!
//! # Default Peers
//!
//! For simple link types, there's only one peer to talk to:
//!
//! | Link Type | Default Peer |
//! |-----------|--------------|
//! | [`ClientToAgent`] | The agent |
//! | [`AgentToClient`] | The client |
//!
//! So when you write:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, Component};
//! # use sacp::schema::{InitializeRequest, ProtocolVersion};
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! # ClientToAgent::builder().run_until(transport, async |cx| {
//! // As a client
//! cx.send_request(InitializeRequest::new(ProtocolVersion::LATEST));
//! # Ok(())
//! # }).await?;
//! # Ok(())
//! # }
//! ```
//!
//! The request automatically goes to the agent, because that's the only peer
//! a client can talk to.
//!
//! # Explicit Peer Methods
//!
//! Every method has an explicit variant that takes a peer argument:
//!
//! | Default method | Explicit variant |
//! |----------------|------------------|
//! | `send_request` | `send_request_to(peer, request)` |
//! | `send_notification` | `send_notification_to(peer, request)` |
//! | `on_receive_request` | `on_receive_request_from(peer, callback)` |
//! | `on_receive_notification` | `on_receive_notification_from(peer, callback)` |
//!
//! For simple links, the explicit form is equivalent:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, AgentPeer, Component};
//! # use sacp_test::MyRequest;
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! # ClientToAgent::builder().run_until(transport, async |cx| {
//! # let req = MyRequest {};
//! // These are equivalent for ClientToAgent:
//! cx.send_request(req.clone());
//! cx.send_request_to(AgentPeer, req);
//! # Ok(())
//! # }).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Why Explicit Peers Matter
//!
//! Explicit peers become essential when working with proxies. A proxy sits
//! between a client and an agent, so it has *two* peers:
//!
//! - [`ClientPeer`] - the client (or previous proxy in the chain)
//! - [`AgentPeer`] - the agent (or next proxy in the chain)
//!
//! When writing proxy code, you need to specify which direction:
//!
//! ```
//! # use sacp::{ProxyToConductor, ClientPeer, AgentPeer, Component};
//! # use sacp::link::ConductorToProxy;
//! # use sacp_test::MyRequest;
//! # async fn example(transport: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
//! ProxyToConductor::builder()
//!     // Receive a request from the client
//!     .on_receive_request_from(ClientPeer, async |req: MyRequest, request_cx, cx| {
//!         // Forward it to the agent
//!         cx.send_request_to(AgentPeer, req)
//!             .forward_to_request_cx(request_cx)
//!     }, sacp::on_receive_request!())
//!     .serve(transport)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! See [Proxies and Conductors](super::proxies) for more on building proxies.
//!
//! # Available Peer Types
//!
//! | Peer Type | Represents |
//! |-----------|------------|
//! | [`ClientPeer`] | The client direction |
//! | [`AgentPeer`] | The agent direction |
//! | [`ConductorPeer`] | The conductor (for proxies) |
//!
//! # Next Steps
//!
//! - [Ordering](super::ordering) - Understand dispatch loop semantics
//! - [Proxies and Conductors](super::proxies) - Build proxies that use explicit peers
//!
//! [`ClientToAgent`]: crate::ClientToAgent
//! [`AgentToClient`]: crate::AgentToClient
//! [`ClientPeer`]: crate::ClientPeer
//! [`AgentPeer`]: crate::AgentPeer
//! [`ConductorPeer`]: crate::ConductorPeer
