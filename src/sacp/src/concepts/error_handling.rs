//! Error handling patterns in sacp.
//!
//! This chapter explains how errors work in sacp callbacks and the difference
//! between *protocol errors* (sent to the peer) and *connection errors* (which
//! shut down the connection).
//!
//! # Callback Return Types
//!
//! Almost all sacp callbacks return `Result<_, crate::Error>`. What happens
//! when you return an `Err` depends on the context:
//!
//! **Returning `Err` from a callback shuts down the connection.**
//!
//! This is appropriate for truly unrecoverable situations—internal bugs,
//! resource exhaustion, or when you want to terminate the connection.
//! But most of the time, you want to send an error *to the peer* while
//! keeping the connection alive.
//!
//! # Sending Protocol Errors
//!
//! To send an error response to a request (without closing the connection),
//! use the request context's `respond` method:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, Component};
//! # use sacp_test::{ValidateRequest, ValidateResponse};
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! ClientToAgent::builder()
//!     .on_receive_request(async |request: ValidateRequest, request_cx, _cx| {
//!         if request.data.is_empty() {
//!             // Send error to peer, keep connection alive
//!             request_cx.respond_with_error(sacp::Error::invalid_params())?;
//!             return Ok(());
//!         }
//!
//!         // Process valid request...
//!         request_cx.respond(ValidateResponse { is_valid: true, error: None })?;
//!         Ok(())
//!     }, sacp::on_receive_request!())
//! #   .run_until(transport, async |_| Ok(())).await?;
//! # Ok(())
//! # }
//! ```
//!
//! For sending error notifications (one-way error messages), use
//! [`send_error_notification`][crate::JrConnectionCx::send_error_notification]:
//!
//! ```
//! # use sacp::{ClientToAgent, AgentToClient, Component};
//! # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
//! # ClientToAgent::builder().run_until(transport, async |cx| {
//! cx.send_error_notification(sacp::Error::internal_error()
//!     .with_data("Something went wrong"))?;
//! # Ok(())
//! # }).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # The `into_internal_error` Helper
//!
//! When working with external libraries that return their own error types,
//! you need to convert them to `sacp::Error`. The
//! [`Error::into_internal_error`][crate::Error::into_internal_error] method
//! provides a convenient way to do this:
//!
//! ```
//! use sacp::Error;
//!
//! # fn example() -> Result<(), sacp::Error> {
//! # let data = "hello";
//! # let path = "/tmp/test.txt";
//! // Convert any error type to sacp::Error
//! let value = serde_json::to_value(&data)
//!     .map_err(Error::into_internal_error)?;
//!
//! // Or with a file operation
//! let contents = std::fs::read_to_string(path)
//!     .map_err(Error::into_internal_error);
//! # Ok(())
//! # }
//! ```
//!
//! This wraps the original error's message in an internal error, which is
//! appropriate for unexpected failures. For expected error conditions that
//! should be communicated to the peer, create specific error types instead.
//!
//! # Error Types
//!
//! The [`Error`][crate::Error] type provides factory methods for common
//! JSON-RPC error codes:
//!
//! - [`Error::parse_error()`][crate::Error::parse_error] - Invalid JSON
//! - [`Error::invalid_request()`][crate::Error::invalid_request] - Malformed request
//! - [`Error::method_not_found()`][crate::Error::method_not_found] - Unknown method
//! - [`Error::invalid_params()`][crate::Error::invalid_params] - Bad parameters
//! - [`Error::internal_error()`][crate::Error::internal_error] - Server error
//!
//! You can add context with `.with_data()`:
//!
//! ```
//! let error = sacp::Error::invalid_params()
//!     .with_data(serde_json::json!({
//!         "field": "timeout",
//!         "reason": "must be positive"
//!     }));
//! ```
//!
//! # Summary
//!
//! | Situation | What to do |
//! |-----------|------------|
//! | Send error response to request | `request_cx.respond(Err(error))` then `Ok(())` |
//! | Send error notification | `cx.send_error_notification(error)` then `Ok(())` |
//! | Shut down connection | Return `Err(error)` from callback |
//! | Convert external error | `.map_err(Error::into_internal_error)?` |
