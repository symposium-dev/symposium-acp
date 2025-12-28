//! Creating and managing sessions for multi-turn conversations.
//!
//! A **session** represents a multi-turn conversation with an agent. Within a
//! session, you can send prompts, receive responses, and the agent maintains
//! context across turns.
//!
//! # Creating a Session
//!
//! Use the session builder to create a new session:
//!
//! ```ignore
//! cx.build_session_cwd()?          // Use current working directory
//!     .block_task()                // Mark as blocking
//!     .run_until(async |session| {
//!         // Use the session here
//!         Ok(())
//!     })
//!     .await?;
//! ```
//!
//! Or specify a custom working directory:
//!
//! ```ignore
//! cx.build_session("/path/to/project")?
//!     .block_task()
//!     .run_until(async |session| { ... })
//!     .await?;
//! ```
//!
//! # Sending Prompts
//!
//! Inside `run_until`, you get an [`ActiveSession`] that lets you interact
//! with the agent:
//!
//! ```ignore
//! .run_until(async |mut session| {
//!     // Send a prompt
//!     session.send_prompt("What is 2 + 2?")?;
//!
//!     // Read the complete response as a string
//!     let response = session.read_to_string().await?;
//!     println!("{}", response);
//!
//!     // Send another prompt in the same session
//!     session.send_prompt("And what is 3 + 3?")?;
//!     let response = session.read_to_string().await?;
//!
//!     Ok(())
//! })
//! ```
//!
//! # Adding MCP Servers
//!
//! You can attach MCP (Model Context Protocol) servers to a session to provide
//! tools to the agent:
//!
//! ```ignore
//! cx.build_session_cwd()?
//!     .with_mcp_server(my_mcp_server)?
//!     .block_task()
//!     .run_until(async |session| { ... })
//!     .await?;
//! ```
//!
//! See the cookbook for detailed MCP server examples.
//!
//! # Non-Blocking Session Start
//!
//! If you're inside an `on_receive_*` callback and need to start a session,
//! use `on_session_start` instead of `block_task().run_until()`:
//!
//! ```ignore
//! builder.on_receive_request(async |req: NewSessionRequest, request_cx, cx| {
//!     cx.build_session_from(req)?
//!         .on_session_start(async |session| {
//!             // Handle the session
//!             Ok(())
//!         })?;
//!     Ok(())
//! }, on_receive_request!());
//! ```
//!
//! This follows the same ordering guarantees as other `on_*` methods - see
//! [Ordering](super::ordering) for details.
//!
//! # Next Steps
//!
//! - [Callbacks](super::callbacks) - Handle incoming requests
//! - [Ordering](super::ordering) - Understand when to use `block_task` vs `on_*`
//!
//! [`ActiveSession`]: crate::ActiveSession
