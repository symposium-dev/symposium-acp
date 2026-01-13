//! Core JSON-RPC server support.

use agent_client_protocol_schema::SessionId;
// Re-export jsonrpcmsg for use in public API
pub use jsonrpcmsg;

// Types re-exported from crate root
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::panic::Location;
use std::pin::pin;
use uuid::Uuid;

use boxfnonce::SendBoxFnOnce;
use futures::channel::{mpsc, oneshot};
use futures::future::{self, BoxFuture, Either};
use futures::{AsyncRead, AsyncWrite, StreamExt};

mod dynamic_handler;
pub(crate) mod handlers;
mod incoming_actor;
mod outgoing_actor;
pub(crate) mod responder;
mod task_actor;
mod transport_actor;

use crate::jsonrpc::dynamic_handler::DynamicHandlerMessage;
pub use crate::jsonrpc::handlers::NullHandler;
use crate::jsonrpc::handlers::{ChainedHandler, NamedHandler};
use crate::jsonrpc::handlers::{MessageHandler, NotificationHandler, RequestHandler};
use crate::jsonrpc::outgoing_actor::{OutgoingMessageTx, send_raw_message};
use crate::jsonrpc::responder::SpawnedResponder;
use crate::jsonrpc::responder::{ChainResponder, JrResponder, NullResponder};
use crate::jsonrpc::task_actor::{Task, TaskTx};
use crate::link::{HasDefaultPeer, HasPeer, JrLink};
use crate::mcp_server::McpServer;
use crate::peer::JrPeer;
use crate::{AgentPeer, ClientPeer, Component};

/// Handlers process incoming JSON-RPC messages on a [`JrConnection`].
///
/// When messages arrive, they flow through a chain of handlers. Each handler can
/// either **claim** the message (handle it) or **decline** it (pass to the next handler).
///
/// # Message Flow
///
/// Messages flow through three layers of handlers in order:
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────────┐
/// │                     Incoming Message                            │
/// └─────────────────────────────────────────────────────────────────┘
///                              │
///                              ▼
/// ┌─────────────────────────────────────────────────────────────────┐
/// │  1. User Handlers (registered via on_receive_request, etc.)     │
/// │     - Tried in registration order                               │
/// │     - First handler to return Handled::Yes claims the message   │
/// └─────────────────────────────────────────────────────────────────┘
///                              │ Handled::No
///                              ▼
/// ┌─────────────────────────────────────────────────────────────────┐
/// │  2. Dynamic Handlers (added at runtime)                         │
/// │     - Used for session-specific message handling                │
/// │     - Added via JrConnectionCx::add_dynamic_handler             │
/// └─────────────────────────────────────────────────────────────────┘
///                              │ Handled::No
///                              ▼
/// ┌─────────────────────────────────────────────────────────────────┐
/// │  3. Role Default Handler                                        │
/// │     - Fallback based on the connection's JrLink                 │
/// │     - Handles protocol-level messages (e.g., proxy forwarding)  │
/// └─────────────────────────────────────────────────────────────────┘
///                              │ Handled::No
///                              ▼
/// ┌─────────────────────────────────────────────────────────────────┐
/// │  Unhandled: Error response sent (or queued if retry=true)       │
/// └─────────────────────────────────────────────────────────────────┘
/// ```
///
/// # The `Handled` Return Value
///
/// Each handler returns [`Handled`] to indicate whether it processed the message:
///
/// - **`Handled::Yes`** - Message was handled. No further handlers are invoked.
/// - **`Handled::No { message, retry }`** - Message was not handled. The message
///   (possibly modified) is passed to the next handler in the chain.
///
/// For convenience, handlers can return `()` which is equivalent to `Handled::Yes`.
///
/// # The Retry Mechanism
///
/// The `retry` flag in `Handled::No` controls what happens when no handler claims a message:
///
/// - **`retry: false`** (default) - Send a "method not found" error response immediately.
/// - **`retry: true`** - Queue the message and retry it when new dynamic handlers are added.
///
/// This mechanism exists because of a timing issue with sessions: when a `session/new`
/// response is being processed, the dynamic handler for that session hasn't been registered
/// yet, but `session/update` notifications for that session may already be arriving.
/// By setting `retry: true`, these early notifications are queued until the session's
/// dynamic handler is added.
///
/// # Handler Registration
///
/// Most users register handlers using the builder methods on [`JrConnectionBuilder`]:
///
/// ```
/// # use sacp::{AgentToClient, ClientToAgent, Component};
/// # use sacp::schema::{InitializeRequest, InitializeResponse, AgentCapabilities};
/// # use sacp_test::StatusUpdate;
/// # async fn example(transport: impl Component<ClientToAgent>) -> Result<(), sacp::Error> {
/// AgentToClient::builder()
///     .on_receive_request(async |req: InitializeRequest, request_cx, cx| {
///         request_cx.respond(
///             InitializeResponse::new(req.protocol_version)
///                 .agent_capabilities(AgentCapabilities::new()),
///         )
///     }, sacp::on_receive_request!())
///     .on_receive_notification(async |notif: StatusUpdate, cx| {
///         // Process notification
///         Ok(())
///     }, sacp::on_receive_notification!())
///     .serve(transport)
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// The type parameter on the closure determines which messages are dispatched to it.
/// Messages that don't match the type are automatically passed to the next handler.
///
/// # Implementing Custom Handlers
///
/// For advanced use cases, you can implement `JrMessageHandler` directly:
///
/// ```ignore
/// struct MyHandler;
///
/// impl JrMessageHandler for MyHandler {
///     type Link = ClientToAgent;
///
///     async fn handle_message(
///         &mut self,
///         message: MessageCx,
///         cx: JrConnectionCx<Self::Role>,
///     ) -> Result<Handled<MessageCx>, Error> {
///         if message.method() == "my/custom/method" {
///             // Handle it
///             Ok(Handled::Yes)
///         } else {
///             // Pass to next handler
///             Ok(Handled::No { message, retry: false })
///         }
///     }
///
///     fn describe_chain(&self) -> impl std::fmt::Debug {
///         "MyHandler"
///     }
/// }
/// ```
///
/// # Important: Handlers Must Not Block
///
/// The connection processes messages on a single async task. While a handler is running,
/// no other messages can be processed. For expensive operations, use [`JrConnectionCx::spawn`]
/// to run work concurrently:
///
/// ```
/// # use sacp::{ClientToAgent, AgentToClient, Component};
/// # use sacp_test::{expensive_operation, ProcessComplete};
/// # async fn example(transport: impl Component<AgentToClient>) -> Result<(), sacp::Error> {
/// # ClientToAgent::builder().run_until(transport, async |cx| {
/// cx.spawn({
///     let connection_cx = cx.clone();
///     async move {
///         let result = expensive_operation("data").await?;
///         connection_cx.send_notification(ProcessComplete { result })?;
///         Ok(())
///     }
/// })?;
/// # Ok(())
/// # }).await?;
/// # Ok(())
/// # }
/// ```
#[allow(async_fn_in_trait)]
/// A handler for incoming JSON-RPC messages.
///
/// This trait is implemented by types that can process incoming messages on a connection.
/// Handlers are registered with a [`JrConnectionBuilder`] and are called in order until
/// one claims the message.
pub trait JrMessageHandler: Send {
    /// The role type for this handler's connection.
    type Link: JrLink;

    /// Attempt to claim an incoming message (request or notification).
    ///
    /// # Important: do not block
    ///
    /// The server will not process new messages until this handler returns.
    /// You should avoid blocking in this callback unless you wish to block the server (e.g., for rate limiting).
    /// The recommended approach to manage expensive operations is to the [`JrConnectionCx::spawn`] method available on the message context.
    ///
    /// # Parameters
    ///
    /// * `message` - The incoming message to handle.
    /// * `cx` - The connection context, used to send messages and access connection state.
    ///
    /// # Returns
    ///
    /// * `Ok(Handled::Yes)` if the message was claimed. It will not be propagated further.
    /// * `Ok(Handled::No(message))` if not; the (possibly changed) message will be passed to the remaining handlers.
    /// * `Err` if an internal error occurs (this will bring down the server).
    fn handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Self::Link>,
    ) -> impl Future<Output = Result<Handled<MessageCx>, crate::Error>> + Send;

    /// Returns a debug description of the registered handlers for diagnostics.
    fn describe_chain(&self) -> impl std::fmt::Debug;
}

impl<H: JrMessageHandler> JrMessageHandler for &mut H {
    type Link = H::Link;

    fn handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Self::Link>,
    ) -> impl Future<Output = Result<Handled<MessageCx>, crate::Error>> + Send {
        H::handle_message(self, message, cx)
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        H::describe_chain(self)
    }
}

/// A JSON-RPC connection that can act as either a server, client, or both.
///
/// `JrConnection` provides a builder-style API for creating JSON-RPC servers and clients.
/// You start by calling `Link::builder()` (e.g., `ClientToAgent::builder()`), then add message
/// handlers, and finally drive the connection with either [`serve`](JrConnectionBuilder::serve)
/// or [`run_until`](JrConnectionBuilder::run_until), providing a component implementation
/// (e.g., [`ByteStreams`] for byte streams).
///
/// # JSON-RPC Primer
///
/// JSON-RPC 2.0 has two fundamental message types:
///
/// * **Requests** - Messages that expect a response. They have an `id` field that gets
///   echoed back in the response so the sender can correlate them.
/// * **Notifications** - Fire-and-forget messages with no `id` field. The sender doesn't
///   expect or receive a response.
///
/// # Type-Driven Message Dispatch
///
/// The handler registration methods use Rust's type system to determine which messages
/// to handle. The type parameter you provide controls what gets dispatched to your handler:
///
/// ## Single Message Types
///
/// The simplest case - handle one specific message type:
///
/// ```no_run
/// # use sacp_test::*;
/// # use sacp::schema::{InitializeRequest, InitializeResponse, SessionNotification};
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// connection
///     .on_receive_request(async |req: InitializeRequest, request_cx, cx| {
///         // Handle only InitializeRequest messages
///         request_cx.respond(InitializeResponse::make())
///     }, sacp::on_receive_request!())
///     .on_receive_notification(async |notif: SessionNotification, cx| {
///         // Handle only SessionUpdate notifications
///         Ok(())
///     }, sacp::on_receive_notification!())
/// # .serve(sacp_test::MockTransport).await?;
/// # Ok(())
/// # }
/// ```
///
/// ## Enum Message Types
///
/// You can also handle multiple related messages with a single handler by defining an enum
/// that implements the appropriate trait ([`JrRequest`] or [`JrNotification`]):
///
/// ```no_run
/// # use sacp_test::*;
/// # use sacp::{JrRequest, JrMessage, UntypedMessage};
/// # use sacp::schema::{InitializeRequest, InitializeResponse, PromptRequest, PromptResponse};
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// // Define an enum for multiple request types
/// #[derive(Debug, Clone)]
/// enum MyRequests {
///     Initialize(InitializeRequest),
///     Prompt(PromptRequest),
/// }
///
/// // Implement JrRequest for your enum
/// # impl JrMessage for MyRequests {
/// #     fn matches_method(_method: &str) -> bool { false }
/// #     fn method(&self) -> &str { "myRequests" }
/// #     fn to_untyped_message(&self) -> Result<UntypedMessage, sacp::Error> { todo!() }
/// #     fn parse_message(_method: &str, _params: &impl serde::Serialize) -> Result<Self, sacp::Error> { Err(sacp::Error::method_not_found()) }
/// # }
/// impl JrRequest for MyRequests { type Response = serde_json::Value; }
///
/// // Handle all variants in one place
/// connection.on_receive_request(async |req: MyRequests, request_cx, cx| {
///     match req {
///         MyRequests::Initialize(init) => { request_cx.respond(serde_json::json!({})) }
///         MyRequests::Prompt(prompt) => { request_cx.respond(serde_json::json!({})) }
///     }
/// }, sacp::on_receive_request!())
/// # .serve(sacp_test::MockTransport).await?;
/// # Ok(())
/// # }
/// ```
///
/// ## Mixed Message Types
///
/// For enums containing both requests AND notifications, use [`on_receive_message`](Self::on_receive_message):
///
/// ```no_run
/// # use sacp_test::*;
/// # use sacp::MessageCx;
/// # use sacp::schema::{InitializeRequest, InitializeResponse, SessionNotification};
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// // on_receive_message receives MessageCx which can be either a request or notification
/// connection.on_receive_message(async |msg: MessageCx<InitializeRequest, SessionNotification>, _cx| {
///     match msg {
///         MessageCx::Request(req, request_cx) => {
///             request_cx.respond(InitializeResponse::make())
///         }
///         MessageCx::Notification(notif) => {
///             Ok(())
///         }
///         MessageCx::Response(result, request_cx) => {
///             // Forward response to its destination
///             request_cx.respond_with_result(result)
///         }
///     }
/// }, sacp::on_receive_message!())
/// # .serve(sacp_test::MockTransport).await?;
/// # Ok(())
/// # }
/// ```
///
/// # Handler Registration
///
/// Register handlers using these methods (listed from most common to most flexible):
///
/// * [`on_receive_request`](Self::on_receive_request) - Handle JSON-RPC requests (messages expecting responses)
/// * [`on_receive_notification`](Self::on_receive_notification) - Handle JSON-RPC notifications (fire-and-forget)
/// * [`on_receive_message`](Self::on_receive_message) - Handle enums containing both requests and notifications
/// * [`with_handler`](Self::with_handler) - Low-level primitive for maximum flexibility
///
/// ## Handler Ordering
///
/// Handlers are tried in the order you register them. The first handler that claims a message
/// (by matching its type) will process it. Subsequent handlers won't see that message:
///
/// ```no_run
/// # use sacp_test::*;
/// # use sacp::{MessageCx, UntypedMessage};
/// # use sacp::schema::{InitializeRequest, InitializeResponse, PromptRequest, PromptResponse};
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// connection
///     .on_receive_request(async |req: InitializeRequest, request_cx, cx| {
///         // This runs first for InitializeRequest
///         request_cx.respond(InitializeResponse::make())
///     }, sacp::on_receive_request!())
///     .on_receive_request(async |req: PromptRequest, request_cx, cx| {
///         // This runs first for PromptRequest
///         request_cx.respond(PromptResponse::make())
///     }, sacp::on_receive_request!())
///     .on_receive_message(async |msg: MessageCx, cx| {
///         // This runs for any message not handled above
///         msg.respond_with_error(sacp::util::internal_error("unknown method"), cx)
///     }, sacp::on_receive_message!())
/// # .serve(sacp_test::MockTransport).await?;
/// # Ok(())
/// # }
/// ```
///
/// # Event Loop and Concurrency
///
/// Understanding the event loop is critical for writing correct handlers.
///
/// ## The Event Loop
///
/// `JrConnection` runs all handler callbacks on a single async task - the event loop.
/// While a handler is running, **the server cannot receive new messages**. This means
/// any blocking or expensive work in your handlers will stall the entire connection.
///
/// To avoid blocking the event loop, use [`JrConnectionCx::spawn`] to offload serious
/// work to concurrent tasks:
///
/// ```no_run
/// # use sacp_test::*;
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// connection.on_receive_request(async |req: AnalyzeRequest, request_cx, cx| {
///     // Clone cx for the spawned task
///     cx.spawn({
///         let connection_cx = cx.clone();
///         async move {
///             let result = expensive_analysis(&req.data).await?;
///             connection_cx.send_notification(AnalysisComplete { result })?;
///             Ok(())
///         }
///     })?;
///
///     // Respond immediately without blocking
///     request_cx.respond(AnalysisStarted { job_id: 42 })
/// }, sacp::on_receive_request!())
/// # .serve(sacp_test::MockTransport).await?;
/// # Ok(())
/// # }
/// ```
///
/// Note that the entire connection runs within one async task, so parallelism must be
/// managed explicitly using [`spawn`](JrConnectionCx::spawn).
///
/// ## The Connection Context
///
/// Handler callbacks receive a context object (`cx`) for interacting with the connection:
///
/// * **For request handlers** - [`JrRequestCx<R>`] provides [`respond`](JrRequestCx::respond)
///   to send the response, plus methods to send other messages
/// * **For notification handlers** - [`JrConnectionCx`] provides methods to send messages
///   and spawn tasks
///
/// Both context types support:
/// * [`send_request`](JrConnectionCx::send_request) - Send requests to the other side
/// * [`send_notification`](JrConnectionCx::send_notification) - Send notifications
/// * [`spawn`](JrConnectionCx::spawn) - Run tasks concurrently without blocking the event loop
///
/// The [`JrResponse`] returned by `send_request` provides methods like
/// [`on_receiving_result`](JrResponse::on_receiving_result) that help you
/// avoid accidentally blocking the event loop while waiting for responses.
///
/// # Driving the Connection
///
/// After adding handlers, you must drive the connection using one of two modes:
///
/// ## Server Mode: `serve()`
///
/// Use [`serve`](Self::serve) when you only need to respond to incoming messages:
///
/// ```no_run
/// # use sacp_test::*;
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// connection
///     .on_receive_request(async |req: MyRequest, request_cx, cx| {
///         request_cx.respond(MyResponse { status: "ok".into() })
///     }, sacp::on_receive_request!())
///     .serve(MockTransport)  // Runs until connection closes or error occurs
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// The connection will process incoming messages and invoke your handlers until the
/// connection is closed or an error occurs.
///
/// ## Client Mode: `run_until()`
///
/// Use [`run_until`](Self::run_until) when you need to both handle incoming messages
/// AND send your own requests/notifications:
///
/// ```no_run
/// # use sacp_test::*;
/// # use sacp::schema::InitializeRequest;
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// connection
///     .on_receive_request(async |req: MyRequest, request_cx, cx| {
///         request_cx.respond(MyResponse { status: "ok".into() })
///     }, sacp::on_receive_request!())
///     .run_until(MockTransport, async |cx| {
///         // You can send requests to the other side
///         let response = cx.send_request(InitializeRequest::make())
///             .block_task()
///             .await?;
///
///         // And send notifications
///         cx.send_notification(StatusUpdate { message: "ready".into() })?;
///
///         Ok(())
///     })
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// The connection will serve incoming messages in the background while your client closure
/// runs. When the closure returns, the connection shuts down.
///
/// # Example: Complete Agent
///
/// ```no_run
/// # use sacp::link::UntypedLink;
/// # use sacp::{JrConnectionBuilder};
/// # use sacp::ByteStreams;
/// # use sacp::schema::{InitializeRequest, InitializeResponse, PromptRequest, PromptResponse, SessionNotification};
/// # use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
/// # async fn example() -> Result<(), sacp::Error> {
/// let transport = ByteStreams::new(
///     tokio::io::stdout().compat_write(),
///     tokio::io::stdin().compat(),
/// );
///
/// UntypedLink::builder()
///     .name("my-agent")  // Optional: for debugging logs
///     .on_receive_request(async |init: InitializeRequest, request_cx, cx| {
///         let response: InitializeResponse = todo!();
///         request_cx.respond(response)
///     }, sacp::on_receive_request!())
///     .on_receive_request(async |prompt: PromptRequest, request_cx, cx| {
///         // You can send notifications while processing a request
///         let notif: SessionNotification = todo!();
///         cx.send_notification(notif)?;
///
///         // Then respond to the request
///         let response: PromptResponse = todo!();
///         request_cx.respond(response)
///     }, sacp::on_receive_request!())
///     .serve(transport)
///     .await?;
/// # Ok(())
/// # }
/// ```
#[must_use]
pub struct JrConnectionBuilder<H: JrMessageHandler, R: JrResponder<H::Link> = NullResponder> {
    name: Option<String>,

    /// Handler for incoming messages.
    handler: H,

    /// Responder for background tasks.
    responder: R,
}

impl<Link: JrLink> JrConnectionBuilder<NullHandler<Link>, NullResponder> {
    /// Create a new JrConnection with the given role.
    /// This type follows a builder pattern; use other methods to configure and then invoke
    /// [`Self::serve`] (to use as a server) or [`Self::run_until`] to use as a client.
    pub(crate) fn new(role: Link) -> Self {
        Self {
            name: Default::default(),
            handler: NullHandler::new(role),
            responder: NullResponder,
        }
    }
}

impl<H: JrMessageHandler> JrConnectionBuilder<H, NullResponder> {
    /// Create a new connection builder with the given handler.
    pub fn new_with(handler: H) -> Self {
        Self {
            name: Default::default(),
            handler,
            responder: NullResponder,
        }
    }
}

impl<H: JrMessageHandler, R: JrResponder<H::Link>> JrConnectionBuilder<H, R> {
    /// Set the "name" of this connection -- used only for debugging logs.
    pub fn name(mut self, name: impl ToString) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Merge another [`JrConnectionBuilder`] into this one.
    ///
    /// Prefer [`Self::on_receive_request`] or [`Self::on_receive_notification`].
    /// This is a low-level method that is not intended for general use.
    pub fn with_connection_builder<H1, R1>(
        self,
        other: JrConnectionBuilder<H1, R1>,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, impl JrResponder<H::Link>>
    where
        H1: JrMessageHandler<Link = H::Link>,
        R1: JrResponder<H::Link>,
    {
        JrConnectionBuilder {
            name: self.name,
            handler: ChainedHandler::new(
                self.handler,
                NamedHandler::new(other.name, other.handler),
            ),
            responder: ChainResponder::new(self.responder, other.responder),
        }
    }

    /// Add a new [`JrMessageHandler`] to the chain.
    ///
    /// Prefer [`Self::on_receive_request`] or [`Self::on_receive_notification`].
    /// This is a low-level method that is not intended for general use.
    pub fn with_handler<H1>(
        self,
        handler: H1,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, R>
    where
        H1: JrMessageHandler<Link = H::Link>,
    {
        JrConnectionBuilder {
            name: self.name,
            handler: ChainedHandler::new(self.handler, handler),
            responder: self.responder,
        }
    }

    /// Add a new [`JrResponder`] to the chain.
    pub fn with_responder<R1>(
        self,
        responder: R1,
    ) -> JrConnectionBuilder<H, impl JrResponder<H::Link>>
    where
        R1: JrResponder<H::Link>,
    {
        JrConnectionBuilder {
            name: self.name,
            handler: self.handler,
            responder: ChainResponder::new(self.responder, responder),
        }
    }

    /// Enqueue a task to run once the connection is actively serving traffic.
    #[track_caller]
    pub fn with_spawned<F, Fut>(self, task: F) -> JrConnectionBuilder<H, impl JrResponder<H::Link>>
    where
        F: FnOnce(JrConnectionCx<H::Link>) -> Fut + Send,
        Fut: Future<Output = Result<(), crate::Error>> + Send,
    {
        let location = Location::caller();
        self.with_responder(SpawnedResponder::new(location, task))
    }

    /// Register a handler for messages that can be either requests OR notifications.
    ///
    /// Use this when you want to handle an enum type that contains both request and
    /// notification variants. Your handler receives a [`MessageCx<Req, Notif>`] which
    /// is an enum with two variants:
    ///
    /// - `MessageCx::Request(request, request_cx)` - A request with its response context
    /// - `MessageCx::Notification(notification)` - A notification
    /// - `MessageCx::Response(result, request_cx)` - A response to a request we sent
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # use sacp::MessageCx;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// # let connection = mock_connection();
    /// connection.on_receive_message(async |message: MessageCx<MyRequest, StatusUpdate>, _cx| {
    ///     match message {
    ///         MessageCx::Request(req, request_cx) => {
    ///             // Handle request and send response
    ///             request_cx.respond(MyResponse { status: "ok".into() })
    ///         }
    ///         MessageCx::Notification(notif) => {
    ///             // Handle notification (no response needed)
    ///             Ok(())
    ///         }
    ///         MessageCx::Response(result, request_cx) => {
    ///             // Forward response to its destination
    ///             request_cx.respond_with_result(result)
    ///         }
    ///     }
    /// }, sacp::on_receive_message!())
    /// # .serve(sacp_test::MockTransport).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// For most use cases, prefer [`on_receive_request`](Self::on_receive_request) or
    /// [`on_receive_notification`](Self::on_receive_notification) which provide cleaner APIs
    /// for handling requests or notifications separately.
    ///
    /// # Ordering
    ///
    /// This callback runs inside the dispatch loop and blocks further message processing
    /// until it completes. See the [`ordering`](crate::concepts::ordering) module for details on
    /// ordering guarantees and how to avoid deadlocks.
    pub fn on_receive_message<Req, Notif, F, T, ToFut>(
        self,
        op: F,
        to_future_hack: ToFut,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, R>
    where
        H::Link: HasDefaultPeer,
        Req: JrRequest,
        Notif: JrNotification,
        F: AsyncFnMut(MessageCx<Req, Notif>, JrConnectionCx<H::Link>) -> Result<T, crate::Error>
            + Send,
        T: IntoHandled<MessageCx<Req, Notif>>,
        ToFut: Fn(
                &mut F,
                MessageCx<Req, Notif>,
                JrConnectionCx<H::Link>,
            ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
            + Send
            + Sync,
    {
        self.with_handler(MessageHandler::new(
            <H::Link as HasDefaultPeer>::DefaultPeer::default(),
            <H::Link>::default(),
            op,
            to_future_hack,
        ))
    }

    /// Register a handler for JSON-RPC requests of type `Req`.
    ///
    /// Your handler receives two arguments:
    /// 1. The request (type `Req`)
    /// 2. A [`JrRequestCx<R, Req::Response>`] for sending the response
    ///
    /// The request context allows you to:
    /// - Send the response with [`JrRequestCx::respond`]
    /// - Send notifications to the client with [`JrConnectionCx::send_notification`]
    /// - Send requests to the client with [`JrConnectionCx::send_request`]
    ///
    /// # Example
    ///
    /// ```ignore
    /// # use sacp::link::UntypedLink;
    /// # use sacp::{JrConnectionBuilder};
    /// # use sacp::schema::{PromptRequest, PromptResponse, SessionNotification};
    /// # fn example(connection: JrConnectionBuilder<impl sacp::JrMessageHandler<Link = UntypedLink>>) {
    /// connection.on_receive_request(async |request: PromptRequest, request_cx, cx| {
    ///     // Send a notification while processing
    ///     let notif: SessionNotification = todo!();
    ///     cx.send_notification(notif)?;
    ///
    ///     // Do some work...
    ///     let result = todo!("process the prompt");
    ///
    ///     // Send the response
    ///     let response: PromptResponse = todo!();
    ///     request_cx.respond(response)
    /// }, sacp::on_receive_request!());
    /// # }
    /// ```
    ///
    /// # Type Parameter
    ///
    /// `Req` can be either a single request type or an enum of multiple request types.
    /// See the [type-driven dispatch](Self#type-driven-message-dispatch) section for details.
    ///
    /// # Ordering
    ///
    /// This callback runs inside the dispatch loop and blocks further message processing
    /// until it completes. See the [`ordering`](crate::concepts::ordering) module for details on
    /// ordering guarantees and how to avoid deadlocks.
    pub fn on_receive_request<Req: JrRequest, F, T, ToFut>(
        self,
        op: F,
        to_future_hack: ToFut,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, R>
    where
        H::Link: HasDefaultPeer,
        F: AsyncFnMut(
                Req,
                JrRequestCx<Req::Response>,
                JrConnectionCx<H::Link>,
            ) -> Result<T, crate::Error>
            + Send,
        T: IntoHandled<(Req, JrRequestCx<Req::Response>)>,
        ToFut: Fn(
                &mut F,
                Req,
                JrRequestCx<Req::Response>,
                JrConnectionCx<H::Link>,
            ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
            + Send
            + Sync,
    {
        self.with_handler(RequestHandler::new(
            <H::Link as HasDefaultPeer>::DefaultPeer::default(),
            <H::Link>::default(),
            op,
            to_future_hack,
        ))
    }

    /// Register a handler for JSON-RPC notifications of type `Notif`.
    ///
    /// Notifications are fire-and-forget messages that don't expect a response.
    /// Your handler receives:
    /// 1. The notification (type `Notif`)
    /// 2. A [`JrConnectionCx<R>`] for sending messages to the other side
    ///
    /// Unlike request handlers, you cannot send a response (notifications don't have IDs),
    /// but you can still send your own requests and notifications using the context.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// # let connection = mock_connection();
    /// connection.on_receive_notification(async |notif: SessionUpdate, cx| {
    ///     // Process the notification
    ///     update_session_state(&notif)?;
    ///
    ///     // Optionally send a notification back
    ///     cx.send_notification(StatusUpdate {
    ///         message: "Acknowledged".into(),
    ///     })?;
    ///
    ///     Ok(())
    /// }, sacp::on_receive_notification!())
    /// # .serve(sacp_test::MockTransport).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Type Parameter
    ///
    /// `Notif` can be either a single notification type or an enum of multiple notification types.
    /// See the [type-driven dispatch](Self#type-driven-message-dispatch) section for details.
    ///
    /// # Ordering
    ///
    /// This callback runs inside the dispatch loop and blocks further message processing
    /// until it completes. See the [`ordering`](crate::concepts::ordering) module for details on
    /// ordering guarantees and how to avoid deadlocks.
    pub fn on_receive_notification<Notif, F, T, ToFut>(
        self,
        op: F,
        to_future_hack: ToFut,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, R>
    where
        H::Link: HasDefaultPeer,
        Notif: JrNotification,
        F: AsyncFnMut(Notif, JrConnectionCx<H::Link>) -> Result<T, crate::Error> + Send,
        T: IntoHandled<(Notif, JrConnectionCx<H::Link>)>,
        ToFut: Fn(
                &mut F,
                Notif,
                JrConnectionCx<H::Link>,
            ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
            + Send
            + Sync,
    {
        self.with_handler(NotificationHandler::new(
            <H::Link as HasDefaultPeer>::DefaultPeer::default(),
            <H::Link>::default(),
            op,
            to_future_hack,
        ))
    }

    /// Register a handler for messages from a specific peer.
    ///
    /// This is similar to [`on_receive_message`](Self::on_receive_message), but allows
    /// specifying the source peer explicitly. This is useful when receiving messages
    /// from a peer that requires message transformation (e.g., unwrapping `SuccessorMessage`
    /// envelopes when receiving from an agent via a proxy).
    ///
    /// For the common case of receiving from the default counterpart, use
    /// [`on_receive_message`](Self::on_receive_message) instead.
    ///
    /// # Ordering
    ///
    /// This callback runs inside the dispatch loop and blocks further message processing
    /// until it completes. See the [`ordering`](crate::concepts::ordering) module for details on
    /// ordering guarantees and how to avoid deadlocks.
    pub fn on_receive_message_from<
        Req: JrRequest,
        Notif: JrNotification,
        Peer: JrPeer,
        F,
        T,
        ToFut,
    >(
        self,
        peer: Peer,
        op: F,
        to_future_hack: ToFut,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, R>
    where
        H::Link: HasPeer<Peer>,
        F: AsyncFnMut(MessageCx<Req, Notif>, JrConnectionCx<H::Link>) -> Result<T, crate::Error>
            + Send,
        T: IntoHandled<MessageCx<Req, Notif>>,
        ToFut: Fn(
                &mut F,
                MessageCx<Req, Notif>,
                JrConnectionCx<H::Link>,
            ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
            + Send
            + Sync,
    {
        self.with_handler(MessageHandler::new(
            peer,
            <H::Link>::default(),
            op,
            to_future_hack,
        ))
    }

    /// Register a handler for JSON-RPC requests from a specific peer.
    ///
    /// This is similar to [`on_receive_request`](Self::on_receive_request), but allows
    /// specifying the source peer explicitly. This is useful when receiving messages
    /// from a peer that requires message transformation (e.g., unwrapping `SuccessorRequest`
    /// envelopes when receiving from an agent via a proxy).
    ///
    /// For the common case of receiving from the default counterpart, use
    /// [`on_receive_request`](Self::on_receive_request) instead.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sacp::Agent;
    /// use sacp::schema::InitializeRequest;
    ///
    /// // Conductor receiving from agent direction - messages will be unwrapped from SuccessorMessage
    /// connection.on_receive_request_from(Agent, async |req: InitializeRequest, request_cx, cx| {
    ///     // Handle the request
    ///     request_cx.respond(InitializeResponse::make())
    /// })
    /// ```
    ///
    /// # Ordering
    ///
    /// This callback runs inside the dispatch loop and blocks further message processing
    /// until it completes. See the [`ordering`](crate::concepts::ordering) module for details on
    /// ordering guarantees and how to avoid deadlocks.
    pub fn on_receive_request_from<Req: JrRequest, Peer: JrPeer, F, T, ToFut>(
        self,
        peer: Peer,
        op: F,
        to_future_hack: ToFut,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, R>
    where
        H::Link: HasPeer<Peer>,
        F: AsyncFnMut(
                Req,
                JrRequestCx<Req::Response>,
                JrConnectionCx<H::Link>,
            ) -> Result<T, crate::Error>
            + Send,
        T: IntoHandled<(Req, JrRequestCx<Req::Response>)>,
        ToFut: Fn(
                &mut F,
                Req,
                JrRequestCx<Req::Response>,
                JrConnectionCx<H::Link>,
            ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
            + Send
            + Sync,
    {
        self.with_handler(RequestHandler::new(
            peer,
            <H::Link>::default(),
            op,
            to_future_hack,
        ))
    }

    /// Register a handler for JSON-RPC notifications from a specific peer.
    ///
    /// This is similar to [`on_receive_notification`](Self::on_receive_notification), but allows
    /// specifying the source peer explicitly. This is useful when receiving messages
    /// from a peer that requires message transformation (e.g., unwrapping `SuccessorNotification`
    /// envelopes when receiving from an agent via a proxy).
    ///
    /// For the common case of receiving from the default counterpart, use
    /// [`on_receive_notification`](Self::on_receive_notification) instead.
    ///
    /// # Ordering
    ///
    /// This callback runs inside the dispatch loop and blocks further message processing
    /// until it completes. See the [`ordering`](crate::concepts::ordering) module for details on
    /// ordering guarantees and how to avoid deadlocks.
    pub fn on_receive_notification_from<Notif: JrNotification, Peer: JrPeer, F, T, ToFut>(
        self,
        peer: Peer,
        op: F,
        to_future_hack: ToFut,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = H::Link>, R>
    where
        H::Link: HasPeer<Peer>,
        F: AsyncFnMut(Notif, JrConnectionCx<H::Link>) -> Result<T, crate::Error> + Send,
        T: IntoHandled<(Notif, JrConnectionCx<H::Link>)>,
        ToFut: Fn(
                &mut F,
                Notif,
                JrConnectionCx<H::Link>,
            ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
            + Send
            + Sync,
    {
        self.with_handler(NotificationHandler::new(
            peer,
            <H::Link>::default(),
            op,
            to_future_hack,
        ))
    }

    /// In a proxy, add this MCP server to new sessions passing through the proxy.
    ///
    /// This adds a handler that intercepts `session/new` requests to include the
    /// registered MCP server.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sacp::mcp_server::McpServiceRegistry;
    /// use sacp::ProxyToConductor;
    ///
    /// ProxyToConductor::builder()
    ///     .name("my-proxy")
    ///     .provide_mcp(McpServiceRegistry::default().with_mcp_server("example", my_server)?)
    ///     .serve(connection)
    ///     .await?;
    /// ```
    pub fn with_mcp_server<Link: JrLink, McpR: JrResponder<Link>>(
        self,
        server: McpServer<Link, McpR>,
    ) -> JrConnectionBuilder<impl JrMessageHandler<Link = Link>, impl JrResponder<Link>>
    where
        H: JrMessageHandler<Link = Link>,
        Link: HasPeer<ClientPeer> + HasPeer<AgentPeer>,
    {
        let (message_handler, mcp_responder) = server.into_handler_and_responder();
        self.with_handler(message_handler)
            .with_responder(mcp_responder)
    }

    /// Connect these handlers to a transport layer.
    /// The resulting connection must then be either [served](`JrConnection::serve`) or [run until completion](`JrConnection::run_until`).
    pub fn connect_to(
        self,
        transport: impl Component<<H::Link as JrLink>::ConnectsTo> + 'static,
    ) -> Result<JrConnection<H, R>, crate::Error> {
        let Self {
            name,
            handler,
            responder,
        } = self;

        let (outgoing_tx, outgoing_rx) = mpsc::unbounded();
        let (new_task_tx, new_task_rx) = mpsc::unbounded();
        let (dynamic_handler_tx, dynamic_handler_rx) = mpsc::unbounded();
        let cx = JrConnectionCx::new(outgoing_tx, new_task_tx, dynamic_handler_tx);

        // Convert transport into server - this returns a channel for us to use
        // and a future that runs the transport
        let transport_component = crate::DynComponent::new(transport);
        let (transport_channel, transport_future) = transport_component.into_server();
        cx.spawn(transport_future)?;

        // Destructure the channel endpoints
        let Channel {
            rx: transport_incoming_rx,
            tx: transport_outgoing_tx,
        } = transport_channel;

        Ok(JrConnection {
            cx,
            name,
            outgoing_rx,
            new_task_rx,
            transport_outgoing_tx,
            transport_incoming_rx,
            dynamic_handler_rx,
            handler,
            responder,
        })
    }

    /// Apply the registered handlers to a single message.
    ///
    /// This method processes one message through all registered handlers, attempting to
    /// match it against each handler in order. This is useful when implementing
    /// custom message handling logic or when you need fine-grained control over message
    /// processing.
    ///
    /// # Returns
    ///
    /// - `Ok(Handled::Claimed)` - A handler claimed and processed the message
    /// - `Ok(Handled::Unclaimed(message))` - No handler matched the message
    /// - `Err(_)` - A handler encountered an error while processing
    ///
    /// # Borrow Checker Considerations
    ///
    /// You may find that [`MatchMessage`](`crate::util::MatchMessage`) is a better choice than this method
    /// for implementing custom handlers. It offers a very similar API to
    /// [`JrConnectionBuilder`] but is structured to apply each test one at a time
    /// (sequentially) instead of setting them all up at once. This sequential approach
    /// often interacts better with the borrow checker, at the cost of requiring `.await`
    /// calls between each handler and only working for processing a single message.
    ///
    /// # Example: Borrow Checker Challenges
    ///
    /// When building a connection with `async {}` blocks (non-move), you might encounter
    /// borrow checker errors if multiple handlers need access to the same mutable state:
    ///
    /// ```compile_fail
    /// # use sacp::{JrConnectionBuilder, JrRequestCx};
    /// # use sacp::schema::{InitializeRequest, InitializeResponse};
    /// # use sacp::schema::{PromptRequest, PromptResponse};
    /// # async fn example() -> Result<(), sacp::Error> {
    /// let mut state = String::from("initial");
    ///
    /// // This fails to compile because both handlers borrow `state` mutably,
    /// // and the futures are set up at the same time (even though only one will run)
    /// let chain = UntypedLink::builder()
    ///     .on_receive_request(async |req: InitializeRequest, cx: JrRequestCx| {
    ///         state.push_str(" - initialized");  // First mutable borrow
    ///         cx.respond(InitializeResponse::make())
    ///     }, sacp::on_receive_request!())
    ///     .on_receive_request(async |req: PromptRequest, cx: JrRequestCx| {
    ///         state.push_str(" - prompted");  // Second mutable borrow - ERROR!
    ///         cx.respond(PromptResponse { content: vec![], stopReason: None })
    ///     }, sacp::on_receive_request!());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// You can work around this by using `apply()` to process messages one at a time,
    /// or use [`MatchMessage`](`crate::util::MatchMessage`) which provides a similar API but applies handlers sequentially:
    ///
    /// ```ignore
    /// use sacp::{MessageCx, Handled};
    /// use sacp::util::MatchMessage;
    ///
    /// async fn handle_with_state(
    ///     message: MessageCx,
    ///     state: &mut String,
    /// ) -> Result<Handled<MessageCx>, sacp::Error> {
    ///     MatchMessage::new(message)
    ///         .on_request(async |req: InitializeRequest, request_cx| {
    ///             state.push_str(" - initialized");  // Sequential - OK!
    ///             request_cx.respond(InitializeResponse::make())
    ///         })
    ///         .on_request(async |req: PromptRequest, request_cx| {
    ///             state.push_str(" - prompted");  // Sequential - OK!
    ///             request_cx.respond(PromptResponse { content: vec![], stopReason: None })
    ///         })
    ///         .otherwise(async |msg| Ok(Handled::Unclaimed(msg)))
    ///         .await
    /// }
    /// ```
    pub async fn apply(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<H::Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        self.handler.handle_message(message, cx).await
    }

    /// Convenience method to connect to a transport and serve.
    ///
    /// This is equivalent to:
    /// ```text
    /// handler_chain.connect_to(transport)?.serve().await
    /// ```
    pub async fn serve(
        self,
        transport: impl Component<<H::Link as JrLink>::ConnectsTo> + 'static,
    ) -> Result<(), crate::Error> {
        self.connect_to(transport)?.serve().await
    }

    /// Convenience method to connect to a transport and run until a closure completes.
    ///
    /// This is equivalent to:
    /// ```text
    /// handler_chain.connect_to(transport)?.run_until(main_fn).await
    /// ```
    pub async fn run_until(
        self,
        transport: impl Component<<H::Link as JrLink>::ConnectsTo> + 'static,
        main_fn: impl AsyncFnOnce(JrConnectionCx<H::Link>) -> Result<(), crate::Error>,
    ) -> Result<(), crate::Error> {
        self.connect_to(transport)?.run_until(main_fn).await
    }
}

impl<H, R> Component<H::Link> for JrConnectionBuilder<H, R>
where
    H: JrMessageHandler + 'static,
    R: JrResponder<H::Link> + 'static,
{
    async fn serve(
        self,
        client: impl Component<<H::Link as JrLink>::ConnectsTo>,
    ) -> Result<(), crate::Error> {
        self.connect_to(client)?.serve().await
    }
}

/// A JSON-RPC connection with an active transport.
///
/// This type represents a `JrConnectionBuilder` that has been connected to a transport
/// via `connect_to()`. It can be driven in two modes:
///
/// - [`serve()`](Self::serve) - Run as a server, handling incoming messages until the connection closes
/// - [`run_until()`](Self::run_until) - Run until a closure completes, allowing you to send requests/notifications
///
/// Most users won't construct this directly - instead use `JrConnectionBuilder::connect_to()` or
/// `JrConnectionBuilder::serve()` for convenience.
pub struct JrConnection<H: JrMessageHandler, R: JrResponder<H::Link> = NullResponder> {
    cx: JrConnectionCx<H::Link>,
    name: Option<String>,
    outgoing_rx: mpsc::UnboundedReceiver<OutgoingMessage>,
    new_task_rx: mpsc::UnboundedReceiver<Task>,
    transport_outgoing_tx: mpsc::UnboundedSender<Result<jsonrpcmsg::Message, crate::Error>>,
    transport_incoming_rx: mpsc::UnboundedReceiver<Result<jsonrpcmsg::Message, crate::Error>>,
    dynamic_handler_rx: mpsc::UnboundedReceiver<DynamicHandlerMessage<H::Link>>,
    handler: H,
    responder: R,
}

impl<H: JrMessageHandler, R: JrResponder<H::Link>> JrConnection<H, R> {
    /// Run the connection in server mode with the provided transport.
    ///
    /// This drives the connection by continuously processing messages from the transport
    /// and dispatching them to your registered handlers. The connection will run until:
    /// - The transport closes (e.g., EOF on byte streams)
    /// - An error occurs
    /// - One of your handlers returns an error
    ///
    /// The transport is responsible for serializing and deserializing `jsonrpcmsg::Message`
    /// values to/from the underlying I/O mechanism (byte streams, channels, etc.).
    ///
    /// Use this mode when you only need to respond to incoming messages and don't need
    /// to initiate your own requests. If you need to send requests to the other side,
    /// use [`run_until`](Self::run_until) instead.
    ///
    /// # Example: Byte Stream Transport
    ///
    /// ```no_run
    /// # use sacp::link::UntypedLink;
    /// # use sacp::{JrConnectionBuilder};
    /// # use sacp::ByteStreams;
    /// # use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// let transport = ByteStreams::new(
    ///     tokio::io::stdout().compat_write(),
    ///     tokio::io::stdin().compat(),
    /// );
    ///
    /// UntypedLink::builder()
    ///     .on_receive_request(async |req: MyRequest, request_cx, cx| {
    ///         request_cx.respond(MyResponse { status: "ok".into() })
    ///     }, sacp::on_receive_request!())
    ///     .serve(transport)
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn serve(self) -> Result<(), crate::Error> {
        self.run_until(async move |_cx| future::pending().await)
            .await
    }

    /// Run the connection until the provided closure completes.
    ///
    /// This drives the connection by:
    /// 1. Running your registered handlers in the background to process incoming messages
    /// 2. Executing your `main_fn` closure with a [`JrConnectionCx<R>`] for sending requests/notifications
    ///
    /// The connection stays active until your `main_fn` returns, then shuts down gracefully.
    /// If the connection closes unexpectedly before `main_fn` completes, this returns an error.
    ///
    /// Use this mode when you need to initiate communication (send requests/notifications)
    /// in addition to responding to incoming messages. For server-only mode where you just
    /// respond to messages, use [`serve`](Self::serve) instead.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use sacp::link::UntypedLink;
    /// # use sacp::{JrConnectionBuilder};
    /// # use sacp::ByteStreams;
    /// # use sacp::schema::InitializeRequest;
    /// # use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// let transport = ByteStreams::new(
    ///     tokio::io::stdout().compat_write(),
    ///     tokio::io::stdin().compat(),
    /// );
    ///
    /// UntypedLink::builder()
    ///     .on_receive_request(async |req: MyRequest, request_cx, cx| {
    ///         // Handle incoming requests in the background
    ///         request_cx.respond(MyResponse { status: "ok".into() })
    ///     }, sacp::on_receive_request!())
    ///     .connect_to(transport)?
    ///     .run_until(async |cx| {
    ///         // Initialize the protocol
    ///         let init_response = cx.send_request(InitializeRequest::make())
    ///             .block_task()
    ///             .await?;
    ///
    ///         // Send more requests...
    ///         let result = cx.send_request(MyRequest {})
    ///             .block_task()
    ///             .await?;
    ///
    ///         // When this closure returns, the connection shuts down
    ///         Ok(())
    ///     })
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Parameters
    ///
    /// - `main_fn`: Your client logic. Receives a [`JrConnectionCx<R>`] for sending messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection closes before `main_fn` completes.
    pub async fn run_until<T>(
        self,
        main_fn: impl AsyncFnOnce(JrConnectionCx<H::Link>) -> Result<T, crate::Error>,
    ) -> Result<T, crate::Error> {
        let JrConnection {
            cx,
            name,
            outgoing_rx,
            new_task_rx,
            handler,
            responder,
            transport_outgoing_tx,
            transport_incoming_rx,
            dynamic_handler_rx,
        } = self;
        let (reply_tx, reply_rx) = mpsc::unbounded();

        crate::util::instrument_with_connection_name(name, async move {
            let background = async {
                futures::try_join!(
                    // Protocol layer: OutgoingMessage → jsonrpcmsg::Message
                    outgoing_actor::outgoing_protocol_actor(
                        outgoing_rx,
                        reply_tx.clone(),
                        transport_outgoing_tx,
                    ),
                    // Protocol layer: jsonrpcmsg::Message → handler/reply routing
                    incoming_actor::incoming_protocol_actor(
                        &cx,
                        transport_incoming_rx,
                        dynamic_handler_rx,
                        reply_rx,
                        handler,
                    ),
                    task_actor::task_actor(new_task_rx, &cx),
                    responder.run(cx.clone()),
                )?;
                Ok(())
            };

            crate::util::run_until(background, main_fn(cx.clone())).await
        })
        .await
    }
}

/// The payload sent through the response oneshot channel.
///
/// Includes the response value and an optional ack channel for dispatch loop
/// synchronization.
pub(crate) struct ResponsePayload {
    /// The response result - either the JSON value or an error.
    pub(crate) result: Result<serde_json::Value, crate::Error>,

    /// Optional acknowledgment channel for dispatch loop synchronization.
    ///
    /// When present, the receiver must send on this channel to signal that
    /// response processing is complete, allowing the dispatch loop to continue
    /// to the next message.
    ///
    /// This is `None` for error paths where the response is sent directly
    /// (e.g., when the outgoing channel is broken) rather than through the
    /// normal dispatch loop flow.
    pub(crate) ack_tx: Option<oneshot::Sender<()>>,
}

impl std::fmt::Debug for ResponsePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponsePayload")
            .field("result", &self.result)
            .field("ack_tx", &self.ack_tx.as_ref().map(|_| "..."))
            .finish()
    }
}

/// Message sent to the incoming actor for reply subscription management.
enum ReplyMessage {
    /// Subscribe to receive a response for the given request id.
    /// When a response with this id arrives, it will be sent through the oneshot
    /// along with an ack channel that must be signaled when processing is complete.
    /// The method name is stored to allow routing responses through typed handlers.
    Subscribe {
        id: jsonrpcmsg::Id,
        method: String,
        sender: oneshot::Sender<ResponsePayload>,
    },
}

impl std::fmt::Debug for ReplyMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplyMessage::Subscribe { id, method, .. } => f
                .debug_struct("Subscribe")
                .field("id", id)
                .field("method", method)
                .finish(),
        }
    }
}

/// Messages send to be serialized over the transport.
#[derive(Debug)]
enum OutgoingMessage {
    /// Send a request to the server.
    Request {
        /// id assigned to this request (generated by sender)
        id: jsonrpcmsg::Id,

        /// method to use in the request
        method: String,

        /// parameters for the request
        params: Option<jsonrpcmsg::Params>,

        /// where to send the response when it arrives (includes ack channel)
        response_tx: oneshot::Sender<ResponsePayload>,
    },

    /// Send a notification to the server.
    Notification {
        /// method to use in the request
        method: String,

        /// parameters for the request
        params: Option<jsonrpcmsg::Params>,
    },

    /// Send a reponse to a message from the server
    Response {
        id: jsonrpcmsg::Id,

        response: Result<serde_json::Value, crate::Error>,
    },

    /// Send a generalized error message
    Error { error: crate::Error },
}

/// Return type from JrHandler; indicates whether the request was handled or not.
#[must_use]
pub enum Handled<T> {
    /// The message was handled
    Yes,

    /// The message was not handled; returns the original value.
    ///
    /// If `retry` is true,
    No {
        /// The message to be passed to subsequent handlers
        /// (typically the original message, but it may have been
        /// mutated.)
        message: T,

        /// If true, request the message to be queued and retried with
        /// dynamic handlers as they are added.
        ///
        /// This is used for managing session updates since the dynamic
        /// handler for a session cannot be added until the response to the
        /// new session request has been processed and there may be updates
        /// that get processed at the same time.
        retry: bool,
    },
}

/// Trait for converting handler return values into [`Handled`].
///
/// This trait allows handlers to return either `()` (which becomes `Handled::Yes`)
/// or an explicit `Handled<T>` value for more control over handler propagation.
pub trait IntoHandled<T> {
    /// Convert this value into a `Handled<T>`.
    fn into_handled(self) -> Handled<T>;
}

impl<T> IntoHandled<T> for () {
    fn into_handled(self) -> Handled<T> {
        Handled::Yes
    }
}

impl<T> IntoHandled<T> for Handled<T> {
    fn into_handled(self) -> Handled<T> {
        self
    }
}

/// Trait for types that can provide transport for JSON-RPC messages.
///
/// Implementations of this trait bridge between the internal protocol channels
/// (which carry `jsonrpcmsg::Message`) and the actual I/O mechanism (byte streams,
/// in-process channels, network sockets, etc.).
///
/// The transport layer is responsible only for moving `jsonrpcmsg::Message` in and out.
/// It has no knowledge of protocol semantics like request/response correlation, ID assignment,
/// or handler dispatch - those are handled by the protocol layer in `JrConnection`.
///
/// # Example
///
/// See [`ByteStreams`] for the standard byte stream implementation.

/// Connection context for sending messages and spawning tasks.
///
/// This is the primary handle for interacting with the JSON-RPC connection from
/// within handler callbacks. You can use it to:
///
/// * Send requests and notifications to the other side
/// * Spawn concurrent tasks that run alongside the connection
/// * Respond to requests (via [`JrRequestCx`] which wraps this)
///
/// # Cloning
///
/// `JrConnectionCx` is cheaply cloneable - all clones refer to the same underlying connection.
/// This makes it easy to share across async tasks.
///
/// # Event Loop and Concurrency
///
/// Handler callbacks run on the event loop, which means the connection cannot process new
/// messages while your handler is running. Use [`spawn`](Self::spawn) to offload any
/// expensive or blocking work to concurrent tasks.
///
/// See the [Event Loop and Concurrency](JrConnection#event-loop-and-concurrency) section
/// for more details.
#[derive(Clone, Debug)]
pub struct JrConnectionCx<Link> {
    #[expect(dead_code)]
    role: Link,
    message_tx: OutgoingMessageTx,
    task_tx: TaskTx,
    dynamic_handler_tx: mpsc::UnboundedSender<DynamicHandlerMessage<Link>>,
}

impl<Link: JrLink> JrConnectionCx<Link> {
    fn new(
        message_tx: mpsc::UnboundedSender<OutgoingMessage>,
        task_tx: mpsc::UnboundedSender<Task>,
        dynamic_handler_tx: mpsc::UnboundedSender<DynamicHandlerMessage<Link>>,
    ) -> Self {
        Self {
            role: Link::default(),
            message_tx,
            task_tx,
            dynamic_handler_tx,
        }
    }

    /// Spawns a task that will run so long as the JSON-RPC connection is being served.
    ///
    /// This is the primary mechanism for offloading expensive work from handler callbacks
    /// to avoid blocking the event loop. Spawned tasks run concurrently with the connection,
    /// allowing the server to continue processing messages.
    ///
    /// # Event Loop
    ///
    /// Handler callbacks run on the event loop, which cannot process new messages while
    /// your handler is running. Use `spawn` for any expensive operations:
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// # let connection = mock_connection();
    /// connection.on_receive_request(async |req: ProcessRequest, request_cx, cx| {
    ///     // Clone cx for the spawned task
    ///     cx.spawn({
    ///         let connection_cx = cx.clone();
    ///         async move {
    ///             let result = expensive_operation(&req.data).await?;
    ///             connection_cx.send_notification(ProcessComplete { result })?;
    ///             Ok(())
    ///         }
    ///     })?;
    ///
    ///     // Respond immediately
    ///     request_cx.respond(ProcessResponse { result: "started".into() })
    /// }, sacp::on_receive_request!())
    /// # .serve(sacp_test::MockTransport).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// If the spawned task returns an error, the entire server will shut down.
    #[track_caller]
    pub fn spawn(
        &self,
        task: impl IntoFuture<Output = Result<(), crate::Error>, IntoFuture: Send + 'static>,
    ) -> Result<(), crate::Error> {
        let location = std::panic::Location::caller();
        let task = task.into_future();
        Task::new(location, task).spawn(&self.task_tx)
    }

    /// Spawn a JSON-RPC connection in the background and return a [`JrConnectionCx`] for sending messages to it.
    ///
    /// This is useful for creating multiple connections that communicate with each other,
    /// such as implementing proxy patterns or connecting to multiple backend services.
    ///
    /// # Arguments
    ///
    /// - `connection`: The `JrConnection` to spawn (typically created via `JrConnectionBuilder::connect_to()`)
    /// - `serve_future`: A function that drives the connection (usually `|c| Box::pin(c.serve())`)
    ///
    /// # Returns
    ///
    /// A `JrConnectionCx` that you can use to send requests and notifications to the spawned connection.
    ///
    /// # Example: Proxying to a backend connection
    ///
    /// ```
    /// # use sacp::link::UntypedLink;
    /// # use sacp::{JrConnectionBuilder, JrConnectionCx};
    /// # use sacp_test::*;
    /// # async fn example(cx: JrConnectionCx<UntypedLink>) -> Result<(), sacp::Error> {
    /// // Set up a backend connection
    /// let backend = UntypedLink::builder()
    ///     .on_receive_request(async |req: MyRequest, request_cx, _cx| {
    ///         request_cx.respond(MyResponse { status: "ok".into() })
    ///     }, sacp::on_receive_request!())
    ///     .connect_to(MockTransport)?;
    ///
    /// // Spawn it and get a context to send requests to it
    /// let backend_cx = cx.spawn_connection(backend, |c| Box::pin(c.serve()))?;
    ///
    /// // Now you can forward requests to the backend
    /// let response = backend_cx.send_request(MyRequest {}).block_task().await?;
    /// # Ok(())
    /// # }
    /// ```
    #[track_caller]
    pub fn spawn_connection<H: JrMessageHandler, R: JrResponder<H::Link> + 'static>(
        &self,
        connection: JrConnection<H, R>,
        serve_future: impl FnOnce(JrConnection<H, R>) -> BoxFuture<'static, Result<(), crate::Error>>,
    ) -> Result<JrConnectionCx<H::Link>, crate::Error> {
        let cx = connection.cx.clone();
        let future = serve_future(connection);
        Task::new(std::panic::Location::caller(), future).spawn(&self.task_tx)?;
        Ok(cx)
    }

    /// Send a request/notification and forward the response appropriately.
    ///
    /// The request context's response type matches the request's response type,
    /// enabling type-safe message forwarding.
    pub fn send_proxied_message_to<
        Peer: JrPeer,
        Req: JrRequest<Response: Send>,
        Notif: JrNotification,
    >(
        &self,
        peer: Peer,
        message: MessageCx<Req, Notif>,
    ) -> Result<(), crate::Error>
    where
        Link: HasPeer<Peer>,
    {
        match message {
            MessageCx::Request(request, request_cx) => self
                .send_request_to(peer, request)
                .forward_to_request_cx(request_cx),
            MessageCx::Notification(notification) => self.send_notification_to(peer, notification),
            MessageCx::Response(result, request_cx) => {
                // Responses are forwarded directly to their destination
                request_cx.respond_with_result(result)
            }
        }
    }

    /// Send an outgoing request and return a [`JrResponse`] for handling the reply.
    ///
    /// The returned [`JrResponse`] provides methods for receiving the response without
    /// blocking the event loop:
    ///
    /// * [`on_receiving_result`](JrResponse::on_receiving_result) - Schedule
    ///   a callback to run when the response arrives (doesn't block the event loop)
    /// * [`block_task`](JrResponse::block_task) - Block the current task until the response
    ///   arrives (only safe in spawned tasks, not in handlers)
    ///
    /// # Anti-Footgun Design
    ///
    /// The API intentionally makes it difficult to block on the result directly to prevent
    /// the common mistake of blocking the event loop while waiting for a response:
    ///
    /// ```compile_fail
    /// # use sacp_test::*;
    /// # async fn example(cx: sacp::JrConnectionCx<sacp::link::UntypedLink>) -> Result<(), sacp::Error> {
    /// // ❌ This doesn't compile - prevents blocking the event loop
    /// let response = cx.send_request(MyRequest {}).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example(cx: sacp::JrConnectionCx<sacp::link::UntypedLink>) -> Result<(), sacp::Error> {
    /// // ✅ Option 1: Schedule callback (safe in handlers)
    /// cx.send_request(MyRequest {})
    ///     .on_receiving_result(async |result| {
    ///         // Handle the response
    ///         Ok(())
    ///     })?;
    ///
    /// // ✅ Option 2: Block in spawned task (safe because task is concurrent)
    /// cx.spawn({
    ///     let cx = cx.clone();
    ///     async move {
    ///         let response = cx.send_request(MyRequest {})
    ///             .block_task()
    ///             .await?;
    ///         // Process response...
    ///         Ok(())
    ///     }
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    /// Send an outgoing request to the default counterpart peer.
    ///
    /// This is a convenience method that uses the connection's default peer.
    /// For explicit control over the target peer, use [`send_request_to`](Self::send_request_to).
    pub fn send_request<Req: JrRequest>(&self, request: Req) -> JrResponse<Req::Response>
    where
        Link: HasDefaultPeer,
    {
        self.send_request_to(Link::DefaultPeer::default(), request)
    }

    /// Send an outgoing request to a specific counterpart peer.
    ///
    /// The message will be transformed according to the link's [`HasPeer`](crate::HasPeer)
    /// implementation before being sent.
    pub fn send_request_to<Peer: JrPeer, Req: JrRequest>(
        &self,
        peer: Peer,
        request: Req,
    ) -> JrResponse<Req::Response>
    where
        Link: HasPeer<Peer>,
    {
        let method = request.method().to_string();
        let id = jsonrpcmsg::Id::String(uuid::Uuid::new_v4().to_string());
        let (response_tx, response_rx) = oneshot::channel();
        match Link::remote_style(peer).transform_outgoing_message(request) {
            Ok(untyped) => {
                // Transform the message for the target role
                let params = crate::util::json_cast(untyped.params).ok();
                let message = OutgoingMessage::Request {
                    id: id.clone(),
                    method: untyped.method.clone(),
                    params,
                    response_tx,
                };

                match self.message_tx.unbounded_send(message) {
                    Ok(()) => (),
                    Err(error) => {
                        let OutgoingMessage::Request {
                            method,
                            response_tx,
                            ..
                        } = error.into_inner()
                        else {
                            unreachable!();
                        };

                        response_tx
                            .send(ResponsePayload {
                                result: Err(crate::util::internal_error(format!(
                                    "failed to send outgoing request `{method}"
                                ))),
                                ack_tx: None,
                            })
                            .unwrap();
                    }
                }
            }

            Err(err) => {
                response_tx
                    .send(ResponsePayload {
                        result: Err(crate::util::internal_error(format!(
                            "failed to create untyped request for `{method}`: {err}"
                        ))),
                        ack_tx: None,
                    })
                    .unwrap();
            }
        }

        JrResponse::new(id, method.clone(), self.task_tx.clone(), response_rx)
            .map(move |json| <Req::Response>::from_value(&method, json))
    }

    /// Send an outgoing notification to the default counterpart peer (no reply expected).
    ///
    /// Notifications are fire-and-forget messages that don't have IDs and don't expect responses.
    /// This method sends the notification immediately and returns.
    ///
    /// This is a convenience method that uses the link's default peer.
    /// For explicit control over the target peer, use [`send_notification_to`](Self::send_notification_to).
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example(cx: sacp::JrConnectionCx<sacp::link::UntypedLink>) -> Result<(), sacp::Error> {
    /// cx.send_notification(StatusUpdate {
    ///     message: "Processing...".into(),
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_notification<N: JrNotification>(&self, notification: N) -> Result<(), crate::Error>
    where
        Link: HasDefaultPeer,
    {
        self.send_notification_to(Link::DefaultPeer::default(), notification)
    }

    /// Send an outgoing notification to a specific counterpart peer (no reply expected).
    ///
    /// The message will be transformed according to the link's [`HasPeer`](crate::HasPeer)
    /// implementation before being sent.
    pub fn send_notification_to<Peer: JrPeer, N: JrNotification>(
        &self,
        peer: Peer,
        notification: N,
    ) -> Result<(), crate::Error>
    where
        Link: HasPeer<Peer>,
    {
        let remote_style = Link::remote_style(peer);
        tracing::debug!(
            role = std::any::type_name::<Link>(),
            peer = std::any::type_name::<Peer>(),
            notification_type = std::any::type_name::<N>(),
            ?remote_style,
            original_method = notification.method(),
            "send_notification_to"
        );
        let transformed = remote_style.transform_outgoing_message(notification)?;
        tracing::debug!(
            transformed_method = %transformed.method,
            "send_notification_to transformed"
        );
        let params = crate::util::json_cast(transformed.params).ok();
        send_raw_message(
            &self.message_tx,
            OutgoingMessage::Notification {
                method: transformed.method,
                params,
            },
        )
    }

    /// Send an error notification (no reply expected).
    pub fn send_error_notification(&self, error: crate::Error) -> Result<(), crate::Error> {
        send_raw_message(&self.message_tx, OutgoingMessage::Error { error })
    }

    /// Register a dynamic message handler, used to intercept messages specific to a particular session
    /// or some similar modal thing.
    ///
    /// Dynamic message handlers are called first for every incoming message.
    ///
    /// If they decline to handle the message, then the message is passed to the regular registered handlers.
    ///
    /// The handler will stay registered until the returned registration guard is dropped.
    pub fn add_dynamic_handler(
        &self,
        handler: impl JrMessageHandler<Link = Link> + 'static,
    ) -> Result<DynamicHandlerRegistration<Link>, crate::Error> {
        let uuid = Uuid::new_v4();
        self.dynamic_handler_tx
            .unbounded_send(DynamicHandlerMessage::AddDynamicHandler(
                uuid.clone(),
                Box::new(handler),
            ))
            .map_err(crate::util::internal_error)?;

        Ok(DynamicHandlerRegistration::new(uuid, self.clone()))
    }

    fn remove_dynamic_handler(&self, uuid: Uuid) {
        match self
            .dynamic_handler_tx
            .unbounded_send(DynamicHandlerMessage::RemoveDynamicHandler(uuid))
        {
            Ok(_) => (),
            Err(_) => ( /* ignore errors */),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DynamicHandlerRegistration<Link: JrLink> {
    uuid: Uuid,
    cx: JrConnectionCx<Link>,
}

impl<Link: JrLink> DynamicHandlerRegistration<Link> {
    fn new(uuid: Uuid, cx: JrConnectionCx<Link>) -> Self {
        Self { uuid, cx }
    }

    /// Prevents the dynamic handler from being removed when dropped.
    pub fn run_indefinitely(self) {
        std::mem::forget(self)
    }
}

impl<Link: JrLink> Drop for DynamicHandlerRegistration<Link> {
    fn drop(&mut self) {
        self.cx.remove_dynamic_handler(self.uuid.clone());
    }
}

/// The context to respond to an incoming request.
///
/// This context is provided to request handlers and serves a dual role:
///
/// 1. **Respond to the request** - Use [`respond`](Self::respond) or
///    [`respond_with_result`](Self::respond_with_result) to send the response
/// 2. **Send other messages** - Use the [`JrConnectionCx`] parameter passed to your
///    handler, which provides [`send_request`](`JrConnectionCx::send_request`),
///    [`send_notification`](`JrConnectionCx::send_notification`), and
///    [`spawn`](`JrConnectionCx::spawn`)
///
/// # Example
///
/// ```no_run
/// # use sacp_test::*;
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// connection.on_receive_request(async |req: ProcessRequest, request_cx, cx| {
///     // Send a notification while processing
///     cx.send_notification(StatusUpdate {
///         message: "processing".into(),
///     })?;
///
///     // Do some work...
///     let result = process(&req.data)?;
///
///     // Respond to the request
///     request_cx.respond(ProcessResponse { result })
/// }, sacp::on_receive_request!())
/// # .serve(sacp_test::MockTransport).await?;
/// # Ok(())
/// # }
/// ```
///
/// # Event Loop Considerations
///
/// Like all handlers, request handlers run on the event loop. Use
/// [`spawn`](JrConnectionCx::spawn) for expensive operations to avoid blocking
/// the connection.
///
/// See the [Event Loop and Concurrency](JrConnection#event-loop-and-concurrency)
/// section for more details.
#[must_use]
pub struct JrRequestCx<T: JrResponsePayload = serde_json::Value> {
    /// The method of the request.
    method: String,

    /// The `id` of the message we are replying to.
    id: jsonrpcmsg::Id,

    /// Function to send the response to its destination.
    ///
    /// For incoming requests: serializes to JSON and sends over the wire.
    /// For incoming responses: sends to the waiting oneshot channel.
    send_fn: SendBoxFnOnce<'static, (Result<T, crate::Error>,), Result<(), crate::Error>>,
}

impl<T: JrResponsePayload> std::fmt::Debug for JrRequestCx<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JrRequestCx")
            .field("method", &self.method)
            .field("id", &self.id)
            .field("response_type", &std::any::type_name::<T>())
            .finish()
    }
}

impl JrRequestCx<serde_json::Value> {
    /// Create a new request context for an incoming request.
    ///
    /// The response will be serialized to JSON and sent over the wire.
    fn new(message_tx: OutgoingMessageTx, method: String, id: jsonrpcmsg::Id) -> Self {
        let id_clone = id.clone();
        Self {
            method,
            id,
            send_fn: SendBoxFnOnce::new(
                move |response: Result<serde_json::Value, crate::Error>| {
                    send_raw_message(
                        &message_tx,
                        OutgoingMessage::Response {
                            id: id_clone,
                            response,
                        },
                    )
                },
            ),
        }
    }

    /// Cast this request context to a different response type.
    ///
    /// The provided type `T` will be serialized to JSON before sending.
    pub fn cast<T: JrResponsePayload>(self) -> JrRequestCx<T> {
        self.wrap_params(move |method, value| match value {
            Ok(value) => T::into_json(value, &method),
            Err(e) => Err(e),
        })
    }
}

impl<T: JrResponsePayload> JrRequestCx<T> {
    /// Method of the incoming request
    pub fn method(&self) -> &str {
        &self.method
    }

    /// ID of the incoming request as a JSON value
    pub fn id(&self) -> serde_json::Value {
        crate::util::id_to_json(&self.id)
    }

    /// Convert to a `JrRequestCx` that expects a JSON value
    /// and which checks (dynamically) that the JSON value it receives
    /// can be converted to `T`.
    pub fn erase_to_json(self) -> JrRequestCx<serde_json::Value> {
        self.wrap_params(|method, value| T::from_value(&method, value?))
    }

    /// Return a new JrRequestCx with a different method name.
    pub fn wrap_method(self, method: String) -> JrRequestCx<T> {
        JrRequestCx {
            method,
            id: self.id,
            send_fn: self.send_fn,
        }
    }

    /// Return a new JrRequestCx that expects a response of type U.
    ///
    /// `wrap_fn` will be invoked with the method name and the result to transform
    /// type `U` into type `T` before sending.
    pub fn wrap_params<U: JrResponsePayload>(
        self,
        wrap_fn: impl FnOnce(&str, Result<U, crate::Error>) -> Result<T, crate::Error> + Send + 'static,
    ) -> JrRequestCx<U> {
        let method = self.method.clone();
        JrRequestCx {
            method: self.method,
            id: self.id,
            send_fn: SendBoxFnOnce::new(move |input: Result<U, crate::Error>| {
                let t_value = wrap_fn(&method, input);
                self.send_fn.call(t_value)
            }),
        }
    }

    /// Respond to the JSON-RPC request with either a value (`Ok`) or an error (`Err`).
    pub fn respond_with_result(
        self,
        response: Result<T, crate::Error>,
    ) -> Result<(), crate::Error> {
        tracing::debug!(id = ?self.id, "respond called");
        self.send_fn.call(response)
    }

    /// Respond to the JSON-RPC request with a value.
    pub fn respond(self, response: T) -> Result<(), crate::Error> {
        self.respond_with_result(Ok(response))
    }

    /// Respond to the JSON-RPC request with an internal error containing a message.
    pub fn respond_with_internal_error(self, message: impl ToString) -> Result<(), crate::Error> {
        self.respond_with_error(crate::util::internal_error(message))
    }

    /// Respond to the JSON-RPC request with an error.
    pub fn respond_with_error(self, error: crate::Error) -> Result<(), crate::Error> {
        tracing::debug!(id = ?self.id, ?error, "respond_with_error called");
        self.respond_with_result(Err(error))
    }
}

/// Context for handling an incoming JSON-RPC response.
///
/// This is the response-side counterpart to [`JrRequestCx`]. While `JrRequestCx` handles
/// incoming requests (where you send a response over the wire), `JrResponseCx` handles
/// incoming responses (where you route the response to a local task waiting for it).
///
/// Both are fundamentally "sinks" that push the message through a `send_fn`, but they
/// represent different points in the message lifecycle and carry different metadata.
#[must_use]
pub struct JrResponseCx<T: JrResponsePayload = serde_json::Value> {
    /// The method of the original request.
    method: String,

    /// The `id` of the original request.
    id: jsonrpcmsg::Id,

    /// Function to send the response to the waiting task.
    send_fn: SendBoxFnOnce<'static, (Result<T, crate::Error>,), Result<(), crate::Error>>,
}

impl<T: JrResponsePayload> std::fmt::Debug for JrResponseCx<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JrResponseCx")
            .field("method", &self.method)
            .field("id", &self.id)
            .field("response_type", &std::any::type_name::<T>())
            .finish()
    }
}

impl JrResponseCx<serde_json::Value> {
    /// Create a new response context for routing a response to a local awaiter.
    ///
    /// When `respond_with_result` is called, the response is sent through the oneshot
    /// channel to the code that originally sent the request.
    pub(crate) fn new(
        method: String,
        id: jsonrpcmsg::Id,
        sender: oneshot::Sender<ResponsePayload>,
    ) -> Self {
        Self {
            method,
            id,
            send_fn: SendBoxFnOnce::new(
                move |response: Result<serde_json::Value, crate::Error>| {
                    sender
                        .send(ResponsePayload {
                            result: response,
                            ack_tx: None,
                        })
                        .map_err(|_| {
                            crate::util::internal_error("failed to send response, receiver dropped")
                        })
                },
            ),
        }
    }

    /// Cast this response context to a different response type.
    ///
    /// The provided type `T` will be serialized to JSON before sending.
    pub fn cast<T: JrResponsePayload>(self) -> JrResponseCx<T> {
        self.wrap_params(move |method, value| match value {
            Ok(value) => T::into_json(value, &method),
            Err(e) => Err(e),
        })
    }
}

impl<T: JrResponsePayload> JrResponseCx<T> {
    /// Method of the original request
    pub fn method(&self) -> &str {
        &self.method
    }

    /// ID of the original request as a JSON value
    pub fn id(&self) -> serde_json::Value {
        crate::util::id_to_json(&self.id)
    }

    /// Convert to a `JrResponseCx` that expects a JSON value
    /// and which checks (dynamically) that the JSON value it receives
    /// can be converted to `T`.
    pub fn erase_to_json(self) -> JrResponseCx<serde_json::Value> {
        self.wrap_params(|method, value| T::from_value(&method, value?))
    }

    /// Return a new JrResponseCx that expects a response of type U.
    ///
    /// `wrap_fn` will be invoked with the method name and the result to transform
    /// type `U` into type `T` before sending.
    fn wrap_params<U: JrResponsePayload>(
        self,
        wrap_fn: impl FnOnce(&str, Result<U, crate::Error>) -> Result<T, crate::Error> + Send + 'static,
    ) -> JrResponseCx<U> {
        let method = self.method.clone();
        JrResponseCx {
            method: self.method,
            id: self.id,
            send_fn: SendBoxFnOnce::new(move |input: Result<U, crate::Error>| {
                let t_value = wrap_fn(&method, input);
                self.send_fn.call(t_value)
            }),
        }
    }

    /// Complete the response by sending the result to the waiting task.
    pub fn respond_with_result(
        self,
        response: Result<T, crate::Error>,
    ) -> Result<(), crate::Error> {
        tracing::debug!(id = ?self.id, "response routed to awaiter");
        self.send_fn.call(response)
    }

    /// Complete the response by sending a value to the waiting task.
    pub fn respond(self, response: T) -> Result<(), crate::Error> {
        self.respond_with_result(Ok(response))
    }

    /// Complete the response by sending an internal error to the waiting task.
    pub fn respond_with_internal_error(self, message: impl ToString) -> Result<(), crate::Error> {
        self.respond_with_error(crate::util::internal_error(message))
    }

    /// Complete the response by sending an error to the waiting task.
    pub fn respond_with_error(self, error: crate::Error) -> Result<(), crate::Error> {
        tracing::debug!(id = ?self.id, ?error, "error routed to awaiter");
        self.respond_with_result(Err(error))
    }
}

/// Common bounds for any JSON-RPC message.
///
/// # Derive Macro
///
/// For simple message types, you can use the `JrRequest` or `JrNotification` derive macros
/// which will implement both `JrMessage` and the respective trait. See [`JrRequest`] and
/// [`JrNotification`] for examples.
pub trait JrMessage: 'static + Debug + Sized + Send + Clone {
    /// Check if this message type matches the given method name.
    fn matches_method(method: &str) -> bool;

    /// The method name for the message.
    fn method(&self) -> &str;

    /// Convert this message into an untyped message.
    fn to_untyped_message(&self) -> Result<UntypedMessage, crate::Error>;

    /// Parse this type from a method name and parameters.
    ///
    /// Returns an error if the method doesn't match or deserialization fails.
    /// Callers should use `matches_method` first to check if this type handles the method.
    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, crate::Error>;
}

/// Defines the "payload" of a successful response to a JSON-RPC request.
///
/// # Derive Macro
///
/// Use `#[derive(JrResponsePayload)]` to automatically implement this trait:
///
/// ```ignore
/// use sacp::JrResponsePayload;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Serialize, Deserialize, JrResponsePayload)]
/// #[response(method = "_hello")]
/// struct HelloResponse {
///     greeting: String,
/// }
/// ```
pub trait JrResponsePayload: 'static + Debug + Sized + Send {
    /// Convert this message into a JSON value.
    fn into_json(self, method: &str) -> Result<serde_json::Value, crate::Error>;

    /// Parse a JSON value into the response type.
    fn from_value(method: &str, value: serde_json::Value) -> Result<Self, crate::Error>;
}

impl JrResponsePayload for serde_json::Value {
    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, crate::Error> {
        Ok(value)
    }

    fn into_json(self, _method: &str) -> Result<serde_json::Value, crate::Error> {
        Ok(self)
    }
}

/// A struct that represents a notification (JSON-RPC message that does not expect a response).
///
/// # Derive Macro
///
/// Use `#[derive(JrNotification)]` to automatically implement both `JrMessage` and `JrNotification`:
///
/// ```ignore
/// use sacp::JrNotification;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, Serialize, Deserialize, JrNotification)]
/// #[notification(method = "_ping")]
/// struct PingNotification {
///     timestamp: u64,
/// }
/// ```
pub trait JrNotification: JrMessage {}

/// A struct that represents a request (JSON-RPC message expecting a response).
///
/// # Derive Macro
///
/// Use `#[derive(JrRequest)]` to automatically implement both `JrMessage` and `JrRequest`:
///
/// ```ignore
/// use sacp::{JrRequest, JrResponsePayload};
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, Serialize, Deserialize, JrRequest)]
/// #[request(method = "_hello", response = HelloResponse)]
/// struct HelloRequest {
///     name: String,
/// }
///
/// #[derive(Debug, Serialize, Deserialize, JrResponsePayload)]
/// struct HelloResponse {
///     greeting: String,
/// }
/// ```
pub trait JrRequest: JrMessage {
    /// The type of data expected in response.
    type Response: JrResponsePayload;
}

/// An enum capturing an in-flight request or notification.
/// In the case of a request, also includes the context used to respond to the request.
///
/// Type parameters allow specifying the concrete request and notification types.
/// By default, both are `UntypedMessage` for dynamic dispatch.
/// The request context's response type matches the request's response type.
#[derive(Debug)]
pub enum MessageCx<Req: JrRequest = UntypedMessage, Notif: JrMessage = UntypedMessage> {
    /// Incoming request and the context where the response should be sent.
    Request(Req, JrRequestCx<Req::Response>),

    /// Incoming notification.
    Notification(Notif),

    /// Incoming response to a request we sent.
    ///
    /// The first field is the response result (success or error from the remote).
    /// The second field is the context for forwarding the response to its destination
    /// (typically a waiting oneshot channel).
    Response(
        Result<Req::Response, crate::Error>,
        JrResponseCx<Req::Response>,
    ),
}

impl<Req: JrRequest, Notif: JrMessage> MessageCx<Req, Notif> {
    /// Map the request and notification types to new types.
    ///
    /// Note: Response variants are passed through unchanged since they don't
    /// contain a parseable message payload.
    pub fn map<Req1, Notif1>(
        self,
        map_request: impl FnOnce(Req, JrRequestCx<Req::Response>) -> (Req1, JrRequestCx<Req1::Response>),
        map_notification: impl FnOnce(Notif) -> Notif1,
    ) -> MessageCx<Req1, Notif1>
    where
        Req1: JrRequest<Response = Req::Response>,
        Notif1: JrMessage,
    {
        match self {
            MessageCx::Request(request, cx) => {
                let (new_request, new_cx) = map_request(request, cx);
                MessageCx::Request(new_request, new_cx)
            }
            MessageCx::Notification(notification) => {
                let new_notification = map_notification(notification);
                MessageCx::Notification(new_notification)
            }
            MessageCx::Response(result, cx) => MessageCx::Response(result, cx),
        }
    }

    /// Respond to the message with an error.
    ///
    /// If this message is a request, this error becomes the reply to the request.
    ///
    /// If this message is a notification, the error is sent as a notification.
    ///
    /// If this message is a response, the error is forwarded to the waiting handler.
    pub fn respond_with_error<Link: JrLink>(
        self,
        error: crate::Error,
        cx: JrConnectionCx<Link>,
    ) -> Result<(), crate::Error> {
        match self {
            MessageCx::Request(_, request_cx) => request_cx.respond_with_error(error),
            MessageCx::Notification(_) => cx.send_error_notification(error),
            MessageCx::Response(_, request_cx) => request_cx.respond_with_error(error),
        }
    }

    /// Convert to a `JrRequestCx` that expects a JSON value
    /// and which checks (dynamically) that the JSON value it receives
    /// can be converted to `T`.
    ///
    /// Note: Response variants cannot be erased since their payload is already
    /// parsed. This returns an error for Response variants.
    pub fn erase_to_json(self) -> Result<MessageCx, crate::Error> {
        match self {
            MessageCx::Request(response, request_cx) => Ok(MessageCx::Request(
                response.to_untyped_message()?,
                request_cx.erase_to_json(),
            )),
            MessageCx::Notification(notification) => {
                Ok(MessageCx::Notification(notification.to_untyped_message()?))
            }
            MessageCx::Response(_, _) => Err(crate::util::internal_error(
                "cannot erase Response variant to JSON",
            )),
        }
    }

    /// Convert the message in self to an untyped message.
    ///
    /// Note: Response variants don't have an untyped message representation.
    /// This returns an error for Response variants.
    pub fn to_untyped_message(&self) -> Result<UntypedMessage, crate::Error> {
        match self {
            MessageCx::Request(request, _) => request.to_untyped_message(),
            MessageCx::Notification(notification) => notification.to_untyped_message(),
            MessageCx::Response(_, _) => Err(crate::util::internal_error(
                "Response variant has no untyped message representation",
            )),
        }
    }

    /// Convert self to an untyped message context.
    ///
    /// Note: Response variants cannot be converted. This returns an error for Response variants.
    pub fn into_untyped_message_cx(self) -> Result<MessageCx, crate::Error> {
        match self {
            MessageCx::Request(request, request_cx) => Ok(MessageCx::Request(
                request.to_untyped_message()?,
                request_cx.erase_to_json(),
            )),
            MessageCx::Notification(notification) => {
                Ok(MessageCx::Notification(notification.to_untyped_message()?))
            }
            MessageCx::Response(_, _) => Err(crate::util::internal_error(
                "cannot convert Response variant to untyped message context",
            )),
        }
    }

    /// Returns the request ID if this is a request or response, None if notification.
    pub fn id(&self) -> Option<serde_json::Value> {
        match self {
            MessageCx::Request(_, cx) => Some(cx.id()),
            MessageCx::Notification(_) => None,
            MessageCx::Response(_, cx) => Some(cx.id()),
        }
    }

    /// Returns the method of the message.
    ///
    /// For requests and notifications, this is the method from the message payload.
    /// For responses, this is the method of the original request.
    pub fn method(&self) -> &str {
        match self {
            MessageCx::Request(msg, _) => msg.method(),
            MessageCx::Notification(msg) => msg.method(),
            MessageCx::Response(_, cx) => cx.method(),
        }
    }
}

impl MessageCx {
    /// Attempts to parse `self` into a typed message context.
    ///
    /// # Returns
    ///
    /// * `Ok(Ok(typed))` if this is a request/notification of the given types
    /// * `Ok(Err(self))` if not
    /// * `Err` if has the correct method for the given types but parsing fails
    pub(crate) fn into_typed_message_cx<Req: JrRequest, Notif: JrNotification>(
        self,
    ) -> Result<Result<MessageCx<Req, Notif>, MessageCx>, crate::Error> {
        match self {
            MessageCx::Request(message, request_cx) => {
                tracing::debug!(
                    request_type = std::any::type_name::<Req>(),
                    message = ?message,
                    "MessageHandler::handle_request"
                );
                if !Req::matches_method(&message.method) {
                    tracing::trace!("MessageHandler::handle_request: method doesn't match");
                    Ok(Err(MessageCx::Request(message, request_cx)))
                } else {
                    match Req::parse_message(&message.method, &message.params) {
                        Ok(req) => {
                            tracing::trace!(
                                ?req,
                                "MessageHandler::handle_request: parse completed"
                            );
                            Ok(Ok(MessageCx::Request(req, request_cx.cast())))
                        }
                        Err(err) => {
                            tracing::trace!(?err, "MessageHandler::handle_request: parse errored");
                            Err(err)
                        }
                    }
                }
            }

            MessageCx::Notification(message) => {
                tracing::debug!(
                    notification_type = std::any::type_name::<Notif>(),
                    message = ?message,
                    "MessageHandler::handle_notification"
                );
                if !Notif::matches_method(&message.method) {
                    tracing::trace!("MessageHandler::handle_notification: method doesn't match");
                    Ok(Err(MessageCx::Notification(message)))
                } else {
                    match Notif::parse_message(&message.method, &message.params) {
                        Ok(notif) => {
                            tracing::trace!(
                                ?notif,
                                "MessageHandler::handle_notification: parse completed"
                            );
                            Ok(Ok(MessageCx::Notification(notif)))
                        }
                        Err(err) => {
                            tracing::trace!(?err, "MessageHandler: parse errored");
                            Err(err)
                        }
                    }
                }
            }

            MessageCx::Response(result, cx) => {
                let method = cx.method();
                tracing::debug!(
                    response_type = std::any::type_name::<Req::Response>(),
                    ?method,
                    "MessageHandler::handle_response"
                );
                if !Req::matches_method(method) {
                    tracing::trace!("MessageHandler::handle_response: method doesn't match");
                    Ok(Err(MessageCx::Response(result, cx)))
                } else {
                    // Parse the response result
                    let typed_result = match result {
                        Ok(value) => {
                            match <Req::Response as JrResponsePayload>::from_value(method, value) {
                                Ok(parsed) => Ok(parsed),
                                Err(err) => {
                                    tracing::trace!(
                                        ?err,
                                        "MessageHandler::handle_response: parse errored"
                                    );
                                    return Err(err);
                                }
                            }
                        }
                        Err(err) => Err(err),
                    };
                    tracing::trace!(
                        ?typed_result,
                        "MessageHandler::handle_response: parse completed"
                    );
                    Ok(Ok(MessageCx::Response(typed_result, cx.cast())))
                }
            }
        }
    }

    /// True if this message has a field with the given name.
    ///
    /// Returns `false` for Response variants.
    pub fn has_field(&self, field_name: &str) -> bool {
        self.message()
            .and_then(|m| m.params().get(field_name))
            .is_some()
    }

    /// Returns true if this message has a session-id field.
    ///
    /// Returns `false` for Response variants.
    pub(crate) fn has_session_id(&self) -> bool {
        self.has_field("sessionId")
    }

    /// Extract the ACP session-id from this message (if any).
    ///
    /// Returns `Ok(None)` for Response variants.
    pub(crate) fn get_session_id(&self) -> Result<Option<SessionId>, crate::Error> {
        let message = match self.message() {
            Some(m) => m,
            None => return Ok(None),
        };
        let value = match message.params().get("sessionId") {
            Some(value) => value,
            None => return Ok(None),
        };
        let session_id = serde_json::from_value(value.clone())?;
        Ok(Some(session_id))
    }

    /// Try to parse this as a notification of the given type.
    ///
    /// # Returns
    ///
    /// * `Ok(Ok(typed))` if this is a request/notification of the given types
    /// * `Ok(Err(self))` if not
    /// * `Err` if has the correct method for the given types but parsing fails
    pub fn into_notification<N: JrNotification>(
        self,
    ) -> Result<Result<N, MessageCx>, crate::Error> {
        match self {
            MessageCx::Request(..) => Ok(Err(self)),
            MessageCx::Notification(msg) => {
                if !N::matches_method(&msg.method) {
                    return Ok(Err(MessageCx::Notification(msg)));
                }
                match N::parse_message(&msg.method, &msg.params) {
                    Ok(n) => Ok(Ok(n)),
                    Err(err) => Err(err),
                }
            }
            MessageCx::Response(..) => Ok(Err(self)),
        }
    }

    /// Try to parse this as a request of the given type.
    ///
    /// # Returns
    ///
    /// * `Ok(Ok(typed))` if this is a request/notification of the given types
    /// * `Ok(Err(self))` if not
    /// * `Err` if has the correct method for the given types but parsing fails
    pub fn into_request<Req: JrRequest>(
        self,
    ) -> Result<Result<(Req, JrRequestCx<Req::Response>), MessageCx>, crate::Error> {
        match self {
            MessageCx::Request(msg, request_cx) => {
                if !Req::matches_method(&msg.method) {
                    return Ok(Err(MessageCx::Request(msg, request_cx)));
                }
                match Req::parse_message(&msg.method, &msg.params) {
                    Ok(req) => Ok(Ok((req, request_cx.cast()))),
                    Err(err) => Err(err),
                }
            }
            MessageCx::Notification(..) => Ok(Err(self)),
            MessageCx::Response(..) => Ok(Err(self)),
        }
    }
}

impl<M: JrRequest + JrNotification> MessageCx<M, M> {
    /// Returns the message payload for requests and notifications.
    ///
    /// Returns `None` for Response variants since they don't contain a message payload.
    pub fn message(&self) -> Option<&M> {
        match self {
            MessageCx::Request(msg, _) => Some(msg),
            MessageCx::Notification(msg) => Some(msg),
            MessageCx::Response(_, _) => None,
        }
    }

    /// Map the request/notification message.
    ///
    /// Response variants pass through unchanged.
    pub(crate) fn try_map_message(
        self,
        map_message: impl FnOnce(M) -> Result<M, crate::Error>,
    ) -> Result<MessageCx<M, M>, crate::Error> {
        match self {
            MessageCx::Request(request, cx) => Ok(MessageCx::Request(map_message(request)?, cx)),
            MessageCx::Notification(notification) => {
                Ok(MessageCx::<M, M>::Notification(map_message(notification)?))
            }
            MessageCx::Response(result, cx) => Ok(MessageCx::Response(result, cx)),
        }
    }
}

/// An incoming JSON message without any typing. Can be a request or a notification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UntypedMessage {
    /// The JSON-RPC method name
    pub method: String,
    /// The JSON-RPC parameters as a raw JSON value
    pub params: serde_json::Value,
}

impl UntypedMessage {
    /// Returns an untyped message with the given method and parameters.
    pub fn new(method: &str, params: impl Serialize) -> Result<Self, crate::Error> {
        let params = serde_json::to_value(params)?;
        Ok(Self {
            method: method.to_string(),
            params,
        })
    }

    /// Returns the method name
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Returns the parameters as a JSON value
    pub fn params(&self) -> &serde_json::Value {
        &self.params
    }

    /// Consumes this message and returns the method and params
    pub fn into_parts(self) -> (String, serde_json::Value) {
        (self.method, self.params)
    }
}

impl JrMessage for UntypedMessage {
    fn matches_method(_method: &str) -> bool {
        // UntypedMessage matches any method - it's the untyped fallback
        true
    }

    fn method(&self) -> &str {
        &self.method
    }

    fn to_untyped_message(&self) -> Result<UntypedMessage, crate::Error> {
        Ok(self.clone())
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, crate::Error> {
        UntypedMessage::new(method, params)
    }
}

impl JrRequest for UntypedMessage {
    type Response = serde_json::Value;
}

impl JrNotification for UntypedMessage {}

/// Represents a pending response of type `R` from an outgoing request.
///
/// Returned by [`JrConnectionCx::send_request`], this type provides methods for handling
/// the response without blocking the event loop. The API is intentionally designed to make
/// it difficult to accidentally block.
///
/// # Anti-Footgun Design
///
/// You cannot directly `.await` a `JrResponse`. Instead, you must choose how to handle
/// the response:
///
/// ## Option 1: Schedule a Callback (Safe in Handlers)
///
/// Use [`on_receiving_result`](Self::on_receiving_result) to schedule a task
/// that runs when the response arrives. This doesn't block the event loop:
///
/// ```no_run
/// # use sacp_test::*;
/// # async fn example(cx: sacp::JrConnectionCx<sacp::link::UntypedLink>) -> Result<(), sacp::Error> {
/// cx.send_request(MyRequest {})
///     .on_receiving_result(async |result| {
///         match result {
///             Ok(response) => {
///                 // Handle successful response
///                 Ok(())
///             }
///             Err(error) => {
///                 // Handle error
///                 Err(error)
///             }
///         }
///     })?;
/// # Ok(())
/// # }
/// ```
///
/// ## Option 2: Block in a Spawned Task (Safe Only in `spawn`)
///
/// Use [`block_task`](Self::block_task) to block until the response arrives, but **only**
/// in a spawned task (never in a handler):
///
/// ```no_run
/// # use sacp_test::*;
/// # async fn example(cx: sacp::JrConnectionCx<sacp::link::UntypedLink>) -> Result<(), sacp::Error> {
/// // ✅ Safe: Spawned task runs concurrently
/// cx.spawn({
///     let cx = cx.clone();
///     async move {
///         let response = cx.send_request(MyRequest {})
///             .block_task()
///             .await?;
///         // Process response...
///         Ok(())
///     }
/// })?;
/// # Ok(())
/// # }
/// ```
///
/// ```no_run
/// # use sacp_test::*;
/// # async fn example() -> Result<(), sacp::Error> {
/// # let connection = mock_connection();
/// // ❌ NEVER do this in a handler - blocks the event loop!
/// connection.on_receive_request(async |req: MyRequest, request_cx, cx| {
///     let response = cx.send_request(MyRequest {})
///         .block_task()  // This will deadlock!
///         .await?;
///     request_cx.respond(response)
/// }, sacp::on_receive_request!())
/// # .serve(sacp_test::MockTransport).await?;
/// # Ok(())
/// # }
/// ```
///
/// # Why This Design?
///
/// If you block the event loop while waiting for a response, the connection cannot process
/// the incoming response message, creating a deadlock. This API design prevents that footgun
/// by making blocking explicit and encouraging non-blocking patterns.
pub struct JrResponse<T> {
    id: jsonrpcmsg::Id,
    method: String,
    task_tx: TaskTx,
    response_rx: oneshot::Receiver<ResponsePayload>,
    to_result: Box<dyn Fn(serde_json::Value) -> Result<T, crate::Error> + Send>,
}

impl JrResponse<serde_json::Value> {
    fn new(
        id: jsonrpcmsg::Id,
        method: String,
        task_tx: mpsc::UnboundedSender<Task>,
        response_rx: oneshot::Receiver<ResponsePayload>,
    ) -> Self {
        Self {
            id,
            method,
            response_rx,
            task_tx,
            to_result: Box::new(Ok),
        }
    }
}

impl<T: JrResponsePayload> JrResponse<T> {
    /// The id of the outgoing request.
    pub fn id(&self) -> serde_json::Value {
        crate::util::id_to_json(&self.id)
    }

    /// The method of the request this is in response to.
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Create a new response that maps the result of the response to a new type.
    pub fn map<U>(
        self,
        map_fn: impl Fn(T) -> Result<U, crate::Error> + 'static + Send,
    ) -> JrResponse<U> {
        JrResponse {
            id: self.id,
            method: self.method,
            response_rx: self.response_rx,
            task_tx: self.task_tx,
            to_result: Box::new(move |value| map_fn((self.to_result)(value)?)),
        }
    }

    /// Forward the response (success or error) to a request context when it arrives.
    ///
    /// This is a convenience method for proxying messages between connections. When the
    /// response arrives, it will be automatically sent to the provided request context,
    /// whether it's a successful response or an error.
    ///
    /// # Example: Proxying requests
    ///
    /// ```
    /// # use sacp::link::UntypedLink;
    /// # use sacp::{JrConnectionBuilder, JrConnectionCx};
    /// # use sacp_test::*;
    /// # async fn example(cx: JrConnectionCx<UntypedLink>) -> Result<(), sacp::Error> {
    /// // Set up backend connection
    /// let backend = UntypedLink::builder()
    ///     .on_receive_request(async |req: MyRequest, request_cx, cx| {
    ///         request_cx.respond(MyResponse { status: "ok".into() })
    ///     }, sacp::on_receive_request!())
    ///     .connect_to(MockTransport)?;
    ///
    /// // Spawn backend and get a context to send to it
    /// let backend_cx = cx.spawn_connection(backend, |c| Box::pin(c.serve()))?;
    ///
    /// // Set up proxy that forwards requests to backend
    /// UntypedLink::builder()
    ///     .on_receive_request({
    ///         let backend_cx = backend_cx.clone();
    ///         async move |req: MyRequest, request_cx, cx| {
    ///             // Forward the request to backend and proxy the response back
    ///             backend_cx.send_request(req)
    ///                 .forward_to_request_cx(request_cx)?;
    ///             Ok(())
    ///         }
    ///     }, sacp::on_receive_request!());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Type Safety
    ///
    /// The request context's response type must match the request's response type,
    /// ensuring type-safe message forwarding.
    ///
    /// # When to Use
    ///
    /// Use this when:
    /// - You're implementing a proxy or gateway pattern
    /// - You want to forward responses without processing them
    /// - The response types match between the outgoing request and incoming request
    ///
    /// This is equivalent to calling `on_receiving_result` and manually forwarding
    /// the result, but more concise.
    pub fn forward_to_request_cx(self, request_cx: JrRequestCx<T>) -> Result<(), crate::Error>
    where
        T: Send,
    {
        self.on_receiving_result(async move |result| request_cx.respond_with_result(result))
    }

    /// Block the current task until the response is received.
    ///
    /// **Warning:** This method blocks the current async task. It is **only safe** to use
    /// in spawned tasks created with [`JrConnectionCx::spawn`]. Using it directly in a
    /// handler callback will deadlock the connection.
    ///
    /// # Safe Usage (in spawned tasks)
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// # let connection = mock_connection();
    /// connection.on_receive_request(async |req: MyRequest, request_cx, cx| {
    ///     // Spawn a task to handle the request
    ///     cx.spawn({
    ///         let connection_cx = cx.clone();
    ///         async move {
    ///             // Safe: We're in a spawned task, not blocking the event loop
    ///             let response = connection_cx.send_request(OtherRequest {})
    ///                 .block_task()
    ///                 .await?;
    ///
    ///             // Process the response...
    ///             Ok(())
    ///         }
    ///     })?;
    ///
    ///     // Respond immediately
    ///     request_cx.respond(MyResponse { status: "ok".into() })
    /// }, sacp::on_receive_request!())
    /// # .serve(sacp_test::MockTransport).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Unsafe Usage (in handlers - will deadlock!)
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// # let connection = mock_connection();
    /// connection.on_receive_request(async |req: MyRequest, request_cx, cx| {
    ///     // ❌ DEADLOCK: Handler blocks event loop, which can't process the response
    ///     let response = cx.send_request(OtherRequest {})
    ///         .block_task()
    ///         .await?;
    ///
    ///     request_cx.respond(MyResponse { status: response.value })
    /// }, sacp::on_receive_request!())
    /// # .serve(sacp_test::MockTransport).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # When to Use
    ///
    /// Use this method when:
    /// - You're in a spawned task (via [`JrConnectionCx::spawn`])
    /// - You need the response value to proceed with your logic
    /// - Linear control flow is more natural than callbacks
    ///
    /// For handler callbacks, use [`on_receiving_result`](Self::on_receiving_result) instead.
    pub async fn block_task(self) -> Result<T, crate::Error>
    where
        T: Send,
    {
        match self.response_rx.await {
            Ok(ResponsePayload {
                result: Ok(json_value),
                ack_tx,
            }) => {
                // Ack immediately - we're in a spawned task, so the dispatch loop
                // can continue while we process the value.
                if let Some(tx) = ack_tx {
                    let _ = tx.send(());
                }
                match (self.to_result)(json_value) {
                    Ok(value) => Ok(value),
                    Err(err) => Err(err),
                }
            }
            Ok(ResponsePayload {
                result: Err(err),
                ack_tx,
            }) => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(());
                }
                Err(err)
            }
            Err(err) => Err(crate::util::internal_error(format!(
                "response to `{}` never received: {}",
                self.method, err
            ))),
        }
    }

    /// Schedule an async task to run when a successful response is received.
    ///
    /// This is a convenience wrapper around [`on_receiving_result`](Self::on_receiving_result)
    /// for the common pattern of forwarding errors to a request context while only processing
    /// successful responses.
    ///
    /// # Behavior
    ///
    /// - If the response is `Ok(value)`, your task receives the value and the request context
    /// - If the response is `Err(error)`, the error is automatically sent to `request_cx`
    ///   and your task is not called
    ///
    /// # Example: Chaining requests
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// # let connection = mock_connection();
    /// connection.on_receive_request(async |req: ValidateRequest, request_cx, cx| {
    ///     // Send initial request
    ///     cx.send_request(ValidateRequest { data: req.data.clone() })
    ///         .on_receiving_ok_result(request_cx, async |validation, request_cx| {
    ///             // Only runs if validation succeeded
    ///             if validation.is_valid {
    ///                 // Respond to original request
    ///                 request_cx.respond(ValidateResponse { is_valid: true, error: None })
    ///             } else {
    ///                 request_cx.respond_with_error(sacp::util::internal_error("validation failed"))
    ///             }
    ///         })?;
    ///
    ///     Ok(())
    /// }, sacp::on_receive_request!())
    /// # .serve(sacp_test::MockTransport).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Ordering
    ///
    /// Like [`on_receiving_result`](Self::on_receiving_result), the callback blocks the
    /// dispatch loop until it completes. See the [`ordering`](crate::concepts::ordering) module
    /// for details.
    ///
    /// # When to Use
    ///
    /// Use this when:
    /// - You need to respond to a request based on another request's result
    /// - You want errors to automatically propagate to the request context
    /// - You only care about the success case
    ///
    /// For more control over error handling, use [`on_receiving_result`](Self::on_receiving_result).
    #[track_caller]
    pub fn on_receiving_ok_result<F>(
        self,
        request_cx: JrRequestCx<T>,
        task: impl FnOnce(T, JrRequestCx<T>) -> F + 'static + Send,
    ) -> Result<(), crate::Error>
    where
        F: Future<Output = Result<(), crate::Error>> + 'static + Send,
        T: Send,
    {
        self.on_receiving_result(async move |result| match result {
            Ok(value) => task(value, request_cx).await,
            Err(err) => request_cx.respond_with_error(err),
        })
    }

    /// Schedule an async task to run when the response is received.
    ///
    /// This is the recommended way to handle responses in handler callbacks, as it doesn't
    /// block the event loop. The task will be spawned automatically when the response arrives.
    ///
    /// # Example: Handle response in callback
    ///
    /// ```no_run
    /// # use sacp_test::*;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// # let connection = mock_connection();
    /// connection.on_receive_request(async |req: MyRequest, request_cx, cx| {
    ///     // Send a request and schedule a callback for the response
    ///     cx.send_request(QueryRequest { id: 22 })
    ///         .on_receiving_result({
    ///             let connection_cx = cx.clone();
    ///             async move |result| {
    ///                 match result {
    ///                     Ok(response) => {
    ///                         println!("Got response: {:?}", response);
    ///                         // Can send more messages here
    ///                         connection_cx.send_notification(QueryComplete {})?;
    ///                         Ok(())
    ///                 }
    ///                     Err(error) => {
    ///                         eprintln!("Request failed: {}", error);
    ///                         Err(error)
    ///                     }
    ///                 }
    ///             }
    ///         })?;
    ///
    ///     // Handler continues immediately without waiting
    ///     request_cx.respond(MyResponse { status: "processing".into() })
    /// }, sacp::on_receive_request!())
    /// # .serve(sacp_test::MockTransport).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Ordering
    ///
    /// The callback runs as a spawned task, but the dispatch loop waits for it to complete
    /// before processing the next message. This gives you ordering guarantees: no other
    /// messages will be processed while your callback runs.
    ///
    /// This differs from [`block_task`](Self::block_task), which signals completion immediately
    /// upon receiving the response (before your code processes it).
    ///
    /// See the [`ordering`](crate::concepts::ordering) module for details on ordering guarantees
    /// and how to avoid deadlocks.
    ///
    /// # Error Handling
    ///
    /// If the scheduled task returns `Err`, the entire server will shut down. Make sure to handle
    /// errors appropriately within your task.
    ///
    /// # When to Use
    ///
    /// Use this method when:
    /// - You're in a handler callback (not a spawned task)
    /// - You want ordering guarantees (no other messages processed during your callback)
    /// - You need to do async work before "releasing" control back to the dispatch loop
    ///
    /// For spawned tasks where you don't need ordering guarantees, consider [`block_task`](Self::block_task).
    #[track_caller]
    pub fn on_receiving_result<F>(
        self,
        task: impl FnOnce(Result<T, crate::Error>) -> F + 'static + Send,
    ) -> Result<(), crate::Error>
    where
        F: Future<Output = Result<(), crate::Error>> + 'static + Send,
        T: Send,
    {
        let task_tx = self.task_tx.clone();
        let method = self.method;
        let response_rx = self.response_rx;
        let to_result = self.to_result;
        let location = Location::caller();

        Task::new(location, async move {
            match response_rx.await {
                Ok(ResponsePayload { result, ack_tx }) => {
                    // Convert the result using to_result for Ok values
                    let typed_result = match result {
                        Ok(json_value) => to_result(json_value),
                        Err(err) => Err(err),
                    };

                    // Run the user's callback
                    let outcome = task(typed_result).await;

                    // Ack AFTER the callback completes - this is the key difference
                    // from block_task. The dispatch loop waits for this ack.
                    if let Some(tx) = ack_tx {
                        let _ = tx.send(());
                    }

                    outcome
                }
                Err(err) => Err(crate::util::internal_error(format!(
                    "response to `{}` never received: {}",
                    method, err
                ))),
            }
        })
        .spawn(&task_tx)
    }
}

// ============================================================================
// IntoJrConnectionTransport Implementations
// ============================================================================

/// A component that communicates over line streams.
///
/// `Lines` implements the [`Component`] trait for any pair of line-based streams
/// (a `Stream<Item = io::Result<String>>` for incoming and a `Sink<String>` for outgoing),
/// handling serialization of JSON-RPC messages to/from newline-delimited JSON.
///
/// This is a lower-level primitive than [`ByteStreams`] that enables interception and
/// transformation of individual lines before they are parsed or after they are serialized.
/// This is particularly useful for debugging, logging, or implementing custom line-based
/// protocols.
///
/// # Use Cases
///
/// - **Line-by-line logging**: Intercept and log each line before parsing
/// - **Custom protocols**: Transform lines before/after JSON-RPC processing
/// - **Debugging**: Inspect raw message strings
/// - **Line filtering**: Skip or modify specific messages
///
/// Most users should use [`ByteStreams`] instead, which provides a simpler interface
/// for byte-based I/O.
///
/// [`Component`]: crate::Component
pub struct Lines<OutgoingSink, IncomingStream> {
    /// Outgoing line sink (where we write serialized JSON-RPC messages)
    pub outgoing: OutgoingSink,
    /// Incoming line stream (where we read and parse JSON-RPC messages)
    pub incoming: IncomingStream,
}

impl<OutgoingSink, IncomingStream> Lines<OutgoingSink, IncomingStream>
where
    OutgoingSink: futures::Sink<String, Error = std::io::Error> + Send + 'static,
    IncomingStream: futures::Stream<Item = std::io::Result<String>> + Send + 'static,
{
    /// Create a new line stream transport.
    pub fn new(outgoing: OutgoingSink, incoming: IncomingStream) -> Self {
        Self { outgoing, incoming }
    }
}

impl<OutgoingSink, IncomingStream, L: JrLink> Component<L> for Lines<OutgoingSink, IncomingStream>
where
    OutgoingSink: futures::Sink<String, Error = std::io::Error> + Send + 'static,
    IncomingStream: futures::Stream<Item = std::io::Result<String>> + Send + 'static,
{
    async fn serve(self, client: impl Component<L::ConnectsTo>) -> Result<(), crate::Error> {
        let (channel, serve_self) = Component::<L>::into_server(self);
        match futures::future::select(Box::pin(client.serve(channel)), serve_self).await {
            Either::Left((result, _)) => result,
            Either::Right((result, _)) => result,
        }
    }

    fn into_server(self) -> (Channel, BoxFuture<'static, Result<(), crate::Error>>) {
        let Self { outgoing, incoming } = self;

        // Create a channel pair for the client to use
        let (channel_for_caller, channel_for_lines) = Channel::duplex();

        // Create the server future that runs the line stream actors
        let server_future = Box::pin(async move {
            let Channel { rx, tx } = channel_for_lines;

            // Run both actors concurrently
            let outgoing_future = transport_actor::transport_outgoing_lines_actor(rx, outgoing);
            let incoming_future = transport_actor::transport_incoming_lines_actor(incoming, tx);

            // Wait for both to complete
            futures::try_join!(outgoing_future, incoming_future)?;

            Ok(())
        });

        (channel_for_caller, server_future)
    }
}

/// A component that communicates over byte streams (stdin/stdout, sockets, pipes, etc.).
///
/// `ByteStreams` implements the [`Component`] trait for any pair of `AsyncRead` and `AsyncWrite`
/// streams, handling serialization of JSON-RPC messages to/from newline-delimited JSON.
/// This is the standard way to communicate with external processes or network connections.
///
/// # Use Cases
///
/// - **Stdio communication**: Connect to agents or proxies via stdin/stdout
/// - **Network sockets**: TCP, Unix domain sockets, or other stream-based protocols
/// - **Named pipes**: Cross-process communication on the same machine
/// - **File I/O**: Reading from and writing to file descriptors
///
/// # Example
///
/// Connecting to an agent via stdio:
///
/// ```no_run
/// use sacp::link::UntypedLink;
/// # use sacp::{ByteStreams};
/// use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
///
/// # async fn example() -> Result<(), sacp::Error> {
/// let component = ByteStreams::new(
///     tokio::io::stdout().compat_write(),
///     tokio::io::stdin().compat(),
/// );
///
/// // Use as a component in a connection
/// sacp::link::UntypedLink::builder()
///     .name("my-client")
///     .serve(component)
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// [`Component`]: crate::Component
pub struct ByteStreams<OB, IB> {
    /// Outgoing byte stream (where we write serialized messages)
    pub outgoing: OB,
    /// Incoming byte stream (where we read and parse messages)
    pub incoming: IB,
}

impl<OB, IB> ByteStreams<OB, IB>
where
    OB: AsyncWrite + Send + 'static,
    IB: AsyncRead + Send + 'static,
{
    /// Create a new byte stream transport.
    pub fn new(outgoing: OB, incoming: IB) -> Self {
        Self { outgoing, incoming }
    }
}

impl<OB, IB, L: JrLink> Component<L> for ByteStreams<OB, IB>
where
    OB: AsyncWrite + Send + 'static,
    IB: AsyncRead + Send + 'static,
{
    async fn serve(self, client: impl Component<L::ConnectsTo>) -> Result<(), crate::Error> {
        let (channel, serve_self) = Component::<L>::into_server(self);
        match futures::future::select(pin!(client.serve(channel)), serve_self).await {
            Either::Left((result, _)) => result,
            Either::Right((result, _)) => result,
        }
    }

    fn into_server(self) -> (Channel, BoxFuture<'static, Result<(), crate::Error>>) {
        use futures::AsyncBufReadExt;
        use futures::AsyncWriteExt;
        use futures::io::BufReader;
        let Self { outgoing, incoming } = self;

        // Convert byte streams to line streams
        // Box both streams to satisfy Unpin requirements
        let incoming_lines = Box::pin(BufReader::new(incoming).lines());

        // Create a sink that writes lines (with newlines) to the outgoing byte stream
        // We need to Box the writer since it may not be Unpin
        let outgoing_sink =
            futures::sink::unfold(Box::pin(outgoing), async move |mut writer, line: String| {
                let mut bytes = line.into_bytes();
                bytes.push(b'\n');
                writer.write_all(&bytes).await?;
                Ok::<_, std::io::Error>(writer)
            });

        // Delegate to Lines component
        Component::<L>::into_server(Lines::new(outgoing_sink, incoming_lines))
    }
}

/// A channel endpoint representing one side of a bidirectional message channel.
///
/// `Channel` represents a single endpoint's view of a bidirectional communication channel.
/// Each endpoint has:
/// - `rx`: A receiver for incoming messages (or errors) from the counterpart
/// - `tx`: A sender for outgoing messages (or errors) to the counterpart
///
/// # Example
///
/// ```no_run
/// # use sacp::link::UntypedLink;
/// # use sacp::{Channel, JrConnectionBuilder};
/// # async fn example() -> Result<(), sacp::Error> {
/// // Create a pair of connected channels
/// let (channel_a, channel_b) = Channel::duplex();
///
/// // Each channel can be used by a different component
/// UntypedLink::builder()
///     .name("connection-a")
///     .serve(channel_a)
///     .await?;
/// # Ok(())
/// # }
/// ```
pub struct Channel {
    /// Receives messages (or errors) from the counterpart.
    pub rx: mpsc::UnboundedReceiver<Result<jsonrpcmsg::Message, crate::Error>>,
    /// Sends messages (or errors) to the counterpart.
    pub tx: mpsc::UnboundedSender<Result<jsonrpcmsg::Message, crate::Error>>,
}

impl Channel {
    /// Create a pair of connected channel endpoints.
    ///
    /// Returns two `Channel` instances that are connected to each other:
    /// - Messages sent via `channel_a.tx` are received on `channel_b.rx`
    /// - Messages sent via `channel_b.tx` are received on `channel_a.rx`
    ///
    /// # Returns
    ///
    /// A tuple `(channel_a, channel_b)` of connected channel endpoints.
    pub fn duplex() -> (Self, Self) {
        // Create channels: A sends Result<Message> which B receives as Message
        let (a_tx, b_rx) = mpsc::unbounded();
        let (b_tx, a_rx) = mpsc::unbounded();

        let channel_a = Self { rx: a_rx, tx: a_tx };
        let channel_b = Self { rx: b_rx, tx: b_tx };

        (channel_a, channel_b)
    }

    /// Copy messages from `rx` to `tx`.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    pub async fn copy(mut self) -> Result<(), crate::Error> {
        while let Some(msg) = self.rx.next().await {
            self.tx
                .unbounded_send(msg)
                .map_err(crate::util::internal_error)?;
        }
        Ok(())
    }
}

impl<L: JrLink> Component<L> for Channel {
    async fn serve(self, client: impl Component<L::ConnectsTo>) -> Result<(), crate::Error> {
        let (client_channel, client_serve) = client.into_server();

        match futures::try_join!(
            Channel {
                rx: client_channel.rx,
                tx: self.tx
            }
            .copy(),
            Channel {
                rx: self.rx,
                tx: client_channel.tx
            }
            .copy(),
            client_serve
        ) {
            Ok(((), (), ())) => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn into_server(self) -> (Channel, BoxFuture<'static, Result<(), crate::Error>>) {
        (self, Box::pin(future::ready(Ok(()))))
    }
}
