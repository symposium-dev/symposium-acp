//! Message ordering, concurrency, and the dispatch loop.
//!
//! This module documents how sacp processes incoming messages and the
//! ordering guarantees you can rely on.
//!
//! # The Dispatch Loop
//!
//! Each connection has a central **dispatch loop** that processes incoming
//! messages one at a time. When a message arrives, it is passed to your
//! handlers in order until one claims it (returns [`Handled::Yes`]).
//!
//! The key property: **the dispatch loop waits for each handler to complete
//! before processing the next message.** This gives you sequential ordering
//! guarantees within a single connection.
//!
//! # Callback Methods: `on_*` Blocks the Loop
//!
//! Methods whose names begin with `on_` register callbacks that run inside
//! the dispatch loop. When your callback is invoked, the loop is blocked
//! until your callback returns.
//!
//! This means:
//! - You can be sure no other messages are processed while your callback runs
//! - You can safely do setup before "releasing" control back to the loop
//! - Messages are processed in the order they arrive
//!
//! For example, with [`on_receive_request`]:
//!
//! ```ignore
//! builder.on_receive_request(|cx, request: MyRequest| async move {
//!     // No other messages are processed until this returns
//!     do_some_setup().await;
//!     cx.respond(MyResponse { ... })?;
//!     Ok(())
//! });
//! ```
//!
//! # Deadlock Risk
//!
//! Because `on_*` callbacks block the dispatch loop, it's easy to create
//! deadlocks. The most common pattern:
//!
//! ```ignore
//! // DEADLOCK: This blocks the loop waiting for a response,
//! // but the response can't arrive because the loop is blocked!
//! builder.on_receive_request(|cx, request: MyRequest| async move {
//!     let response = cx.send_request_to(AgentPeer, SomeRequest { ... }).await?;
//!     //            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//!     //            This will never complete - the dispatch loop is blocked
//!     Ok(())
//! });
//! ```
//!
//! # Escaping the Loop: `spawn` and `block_task`
//!
//! To avoid deadlocks, you have two options:
//!
//! ## Option 1: `spawn` (Explicitly Loses Ordering)
//!
//! Use [`spawn`] to run work outside the dispatch loop. The callback returns
//! immediately, and the spawned task runs concurrently with message processing:
//!
//! ```ignore
//! builder.on_receive_request(|cx, request: MyRequest| async move {
//!     cx.spawn(async move {
//!         // This runs outside the loop - other messages may be processed
//!         // while this is running
//!         let response = cx.send_request_to(AgentPeer, SomeRequest { ... }).await?;
//!         // ...
//!         Ok(())
//!     })?;
//!     Ok(()) // Returns immediately, unblocking the loop
//! });
//! ```
//!
//! ## Option 2: `block_task` (For Methods That Support It)
//!
//! Some APIs support the [`block_task`] pattern. This converts an operation
//! into one that will block the *current task* (not the dispatch loop):
//!
//! ```ignore
//! builder.on_receive_request(|cx, request: NewSessionRequest| async move {
//!     cx.build_session_from(request)
//!         .with_mcp_server(mcp)?
//!         .block_task() // Mark that we want to block this task
//!         .run_until(|session| async move {
//!             // Safe to await here - we're in a spawned task
//!             session.send_prompt(prompt).await?;
//!             Ok(())
//!         })
//!         .await
//! });
//! ```
//!
//! The `block_task()` call internally spawns work onto a separate task,
//! so the dispatch loop continues processing messages while your operation
//! runs.
//!
//! # `run_until` Methods
//!
//! Methods named `run_until` (like [`SessionBuilder::run_until`]) are designed
//! to be safely blocked. They run concurrently with the dispatch loop in a
//! spawned task, so awaiting them won't cause deadlocks.
//!
//! The key difference from `on_*` callbacks:
//! - `on_*` runs *inside* the dispatch loop (blocking it)
//! - `run_until` runs *alongside* the dispatch loop (spawned task)
//!
//! # Summary
//!
//! | Pattern | Blocks Loop? | Ordering Guarantee |
//! |---------|--------------|-------------------|
//! | `on_*` callback | Yes | Messages processed in order |
//! | `spawn(...)` | No | Concurrent, no ordering |
//! | `block_task().run_until(...)` | No | Concurrent, no ordering |
//!
//! When in doubt:
//! - Use `on_*` for quick, synchronous decisions (routing, filtering)
//! - Use `spawn` when you need async work but don't need ordering
//! - Use `block_task().run_until()` for session-scoped work
//!
//! [`Handled::Yes`]: crate::jsonrpc::Handled
//! [`on_receive_request`]: crate::JrConnectionBuilder::on_receive_request
//! [`spawn`]: crate::JrConnectionCx::spawn
//! [`block_task`]: crate::SessionBuilder::block_task
//! [`SessionBuilder::run_until`]: crate::SessionBuilder::run_until
