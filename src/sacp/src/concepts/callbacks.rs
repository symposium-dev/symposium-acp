//! Handling incoming messages with `on_receive_*` callbacks.
//!
//! So far we've seen how to *send* messages. But ACP is bidirectional - the
//! remote peer can also send messages to you. Use callbacks to handle them.
//!
//! # Handling Requests
//!
//! Use `on_receive_request` to handle incoming requests that expect a response:
//!
//! ```
//! # use sacp::{Client, Agent, ConnectTo};
//! # use sacp_test::{ValidateRequest, ValidateResponse};
//! # async fn example(transport: impl ConnectTo<Client>) -> Result<(), sacp::Error> {
//! Client.connect_from()
//!     .on_receive_request(async |req: ValidateRequest, request_cx, cx| {
//!         // Process the request
//!         let is_valid = req.data.len() > 0;
//!
//!         // Send the response
//!         request_cx.respond(ValidateResponse { is_valid, error: None })
//!     }, sacp::on_receive_request!())
//!     .connect_with(transport, async |cx| { Ok(()) })
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! Your callback receives three arguments:
//! - The request payload (e.g., `PermissionRequest`)
//! - A [`Responder`] for sending the response
//! - A [`ConnectionTo`] for sending other messages
//!
//! # Handling Notifications
//!
//! Use `on_receive_notification` for fire-and-forget messages that don't need
//! a response:
//!
//! ```
//! # use sacp::{Client, Agent, ConnectTo};
//! # use sacp_test::StatusUpdate;
//! # async fn example(transport: impl ConnectTo<Client>) -> Result<(), sacp::Error> {
//! Client.connect_from()
//!     .on_receive_notification(async |notif: StatusUpdate, cx| {
//!         println!("Status: {}", notif.message);
//!         Ok(())
//!     }, sacp::on_receive_notification!())
//! #   .connect_with(transport, async |_| Ok(())).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # The Request Context
//!
//! The [`Responder`] lets you send a response to the request:
//!
//! ```
//! # use sacp::{Client, Agent, ConnectTo};
//! # use sacp_test::{MyRequest, MyResponse};
//! # async fn example(transport: impl ConnectTo<Client>) -> Result<(), sacp::Error> {
//! # Client.connect_from()
//! #   .on_receive_request(async |req: MyRequest, request_cx, cx| {
//! // Send a successful response
//! request_cx.respond(MyResponse { status: "ok".into() })?;
//! # Ok(())
//! #   }, sacp::on_receive_request!())
//! #   .connect_with(transport, async |_| Ok(())).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Or send an error:
//!
//! ```
//! # use sacp::{Client, Agent, ConnectTo};
//! # use sacp_test::{MyRequest, MyResponse};
//! # async fn example(transport: impl ConnectTo<Client>) -> Result<(), sacp::Error> {
//! # Client.connect_from()
//! #   .on_receive_request(async |req: MyRequest, request_cx, cx| {
//! request_cx.respond_with_error(sacp::Error::invalid_params())?;
//! # Ok(())
//! #   }, sacp::on_receive_request!())
//! #   .connect_with(transport, async |_| Ok(())).await?;
//! # Ok(())
//! # }
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
//! ```
//! # use sacp::{Client, Agent, ConnectTo};
//! # use sacp_test::{ValidateRequest, ValidateResponse, ExecuteRequest, ExecuteResponse};
//! # async fn example(transport: impl ConnectTo<Client>) -> Result<(), sacp::Error> {
//! Client.connect_from()
//!     .on_receive_request(async |req: ValidateRequest, request_cx, cx| {
//!         // Handle validation requests
//!         request_cx.respond(ValidateResponse { is_valid: true, error: None })
//!     }, sacp::on_receive_request!())
//!     .on_receive_request(async |req: ExecuteRequest, request_cx, cx| {
//!         // Handle execution requests
//!         request_cx.respond(ExecuteResponse { result: "done".into() })
//!     }, sacp::on_receive_request!())
//! #   .connect_with(transport, async |_| Ok(())).await?;
//! # Ok(())
//! # }
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
//! [`Responder`]: crate::Responder
//! [`ConnectionTo`]: crate::ConnectionTo
