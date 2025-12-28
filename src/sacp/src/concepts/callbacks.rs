//! Handling incoming messages with `on_receive_*` callbacks.
//!
//! So far we've seen how to *send* messages. But ACP is bidirectional - the
//! remote peer can also send messages to you. Use callbacks to handle them.
//!
//! # Handling Requests
//!
//! Use `on_receive_request` to handle incoming requests that expect a response:
//!
//! ```ignore
//! ClientToAgent::builder()
//!     .on_receive_request(async |req: PermissionRequest, request_cx, cx| {
//!         // Decide whether to grant the permission
//!         let granted = prompt_user(&req).await;
//!
//!         // Send the response
//!         request_cx.respond(PermissionResponse { granted })
//!     }, on_receive_request!())
//!     .run_until(transport, async |cx| { ... })
//!     .await?;
//! ```
//!
//! Your callback receives three arguments:
//! - The request payload (e.g., `PermissionRequest`)
//! - A [`JrRequestCx`] for sending the response
//! - A [`JrConnectionCx`] for sending other messages
//!
//! # Handling Notifications
//!
//! Use `on_receive_notification` for fire-and-forget messages that don't need
//! a response:
//!
//! ```ignore
//! builder.on_receive_notification(async |notif: ProgressNotification, cx| {
//!     println!("Progress: {}%", notif.percent);
//!     Ok(())
//! }, on_receive_notification!());
//! ```
//!
//! # The Request Context
//!
//! The [`JrRequestCx`] lets you send a response to the request:
//!
//! ```ignore
//! // Send a successful response
//! request_cx.respond(MyResponse { ... })?;
//!
//! // Or send an error
//! request_cx.respond_with_error(Error::invalid_params("missing field"))?;
//! ```
//!
//! You must send exactly one response per request. If your callback returns
//! without responding, an error response is sent automatically.
//!
//! # Multiple Handlers
//!
//! You can register multiple handlers. They're tried in order until one
//! handles the message:
//!
//! ```ignore
//! builder
//!     .on_receive_request(async |req: PermissionRequest, request_cx, cx| {
//!         // Handle permission requests
//!         request_cx.respond(PermissionResponse { granted: true })
//!     }, on_receive_request!())
//!     .on_receive_request(async |req: ToolCallRequest, request_cx, cx| {
//!         // Handle tool calls
//!         request_cx.respond(ToolCallResponse { result: "done".into() })
//!     }, on_receive_request!())
//!     // ...
//! ```
//!
//! # Ordering Guarantees
//!
//! Callbacks run inside the dispatch loop and block further message processing
//! until they complete. This gives you ordering guarantees but also means you
//! need to be careful about deadlocks.
//!
//! See [Ordering](super::ordering) for the full details.
//!
//! # Next Steps
//!
//! - [Explicit Peers](super::peers) - Use `_from` variants to specify the source peer
//! - [Ordering](super::ordering) - Understand dispatch loop semantics
//!
//! [`JrRequestCx`]: crate::JrRequestCx
//! [`JrConnectionCx`]: crate::JrConnectionCx
