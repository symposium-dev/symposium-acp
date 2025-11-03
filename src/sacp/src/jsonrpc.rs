//! Core JSON-RPC server support.

// Types re-exported from crate root
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::panic::Location;

use boxfnonce::SendBoxFnOnce;
use futures::channel::{mpsc, oneshot};
use futures::future::Either;
use futures::{AsyncRead, AsyncWrite, FutureExt};

mod actors;
mod handlers;
pub use handlers::*;

use crate::jsonrpc::actors::Task;

/// A JSON-RPC connection that can act as either a server, client, or both.
///
/// `JrConnection` provides a builder-style API for creating JSON-RPC servers and clients.
/// You start by calling [`JrConnection::new`] with your input/output streams, then add
/// message handlers, and finally drive the connection with either [`serve`](Self::serve)
/// or [`with_client`](Self::with_client).
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
/// ```rust,ignore
/// connection
///     .on_receive_request(async |req: InitializeRequest, cx| {
///         // Handle only InitializeRequest messages
///         cx.respond(InitializeResponse { ... })
///     })
///     .on_receive_notification(async |notif: SessionUpdate, cx| {
///         // Handle only SessionUpdate notifications
///         Ok(())
///     })
/// ```
///
/// ## Enum Message Types
///
/// You can also handle multiple related messages with a single handler by defining an enum
/// that implements the appropriate trait ([`JsonRpcRequest`] or [`JrNotification`]):
///
/// ```rust,ignore
/// // Define an enum for multiple request types
/// enum MyRequests {
///     Initialize(InitializeRequest),
///     Prompt(PromptRequest),
/// }
///
/// // Implement JsonRpcRequest for your enum
/// impl JsonRpcRequest for MyRequests { /* ... */ }
///
/// // Handle all variants in one place
/// connection.on_receive_request(async |req: MyRequests, cx| {
///     match req {
///         MyRequests::Initialize(init) => { /* ... */ }
///         MyRequests::Prompt(prompt) => { /* ... */ }
///     }
/// })
/// ```
///
/// ## Mixed Message Types
///
/// For enums containing both requests AND notifications, use [`on_receive_message`](Self::on_receive_message):
///
/// ```rust,ignore
/// enum AllMessages {
///     Request(MyRequests),
///     Notification(MyNotifications),
/// }
///
/// connection.on_receive_message(async |msg: AllMessages, cx| {
///     match msg {
///         AllMessages::Request(req, request_cx) => { /* handle and respond */ }
///         AllMessages::Notification(notif, _) => { /* handle notification */ }
///     }
/// })
/// ```
///
/// # Handler Registration
///
/// Register handlers using these methods (listed from most common to most flexible):
///
/// * [`on_receive_request`](Self::on_receive_request) - Handle JSON-RPC requests (messages expecting responses)
/// * [`on_receive_notification`](Self::on_receive_notification) - Handle JSON-RPC notifications (fire-and-forget)
/// * [`on_receive_message`](Self::on_receive_message) - Handle enums containing both requests and notifications
/// * [`chain_handler`](Self::chain_handler) - Low-level primitive for maximum flexibility
///
/// ## Handler Ordering
///
/// Handlers are tried in the order you register them. The first handler that claims a message
/// (by matching its type) will process it. Subsequent handlers won't see that message:
///
/// ```rust,ignore
/// connection
///     .on_receive_request(async |req: InitializeRequest, cx| {
///         // This runs first for InitializeRequest
///     })
///     .on_receive_request(async |req: PromptRequest, cx| {
///         // This runs first for PromptRequest
///     })
///     .on_receive_message(async |msg: FallbackMessages, cx| {
///         // This runs for any message not handled above
///     })
/// ```
///
/// # Driving the Connection
///
/// After adding handlers, you must drive the connection using one of two modes:
///
/// ## Server Mode: `serve()`
///
/// Use [`serve`](Self::serve) when you only need to respond to incoming messages:
///
/// ```rust,ignore
/// connection
///     .on_receive_request(async |req: MyRequest, cx| { /* ... */ })
///     .serve()  // Runs until connection closes or error occurs
///     .await?;
/// ```
///
/// The connection will process incoming messages and invoke your handlers until the
/// connection is closed or an error occurs.
///
/// ## Client Mode: `with_client()`
///
/// Use [`with_client`](Self::with_client) when you need to both handle incoming messages
/// AND send your own requests/notifications:
///
/// ```rust,ignore
/// connection
///     .on_receive_request(async |req: MyRequest, cx| { /* ... */ })
///     .with_client(async |cx| {
///         // You can send requests to the other side
///         let response = cx.send_request(InitializeRequest { ... })
///             .block_task()
///             .await?;
///
///         // And send notifications
///         cx.send_notification(MyNotification { ... })?;
///
///         Ok(())
///     })
///     .await?;
/// ```
///
/// The connection will serve incoming messages in the background while your client closure
/// runs. When the closure returns, the connection shuts down.
///
/// # Example: Complete Agent
///
/// ```no_run
/// # use sacp::{JrConnection, InitializeRequest, InitializeResponse, PromptRequest, PromptResponse, SessionNotification};
/// # use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
/// # async fn example() -> Result<(), sacp::Error> {
/// let stdout = tokio::io::stdout().compat_write();
/// let stdin = tokio::io::stdin().compat();
/// JrConnection::new(stdout, stdin)
///     .name("my-agent")  // Optional: for debugging logs
///     .on_receive_request(async |init: InitializeRequest, cx| {
///         let response: InitializeResponse = todo!();
///         cx.respond(response)
///     })
///     .on_receive_request(async |prompt: PromptRequest, cx| {
///         // You can send notifications while processing a request
///         let notif: SessionNotification = todo!();
///         cx.send_notification(notif)?;
///
///         // Then respond to the request
///         let response: PromptResponse = todo!();
///         cx.respond(response)
///     })
///     .serve()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[must_use]
pub struct JrConnection<OB: AsyncWrite, IB: AsyncRead, H: JrHandler> {
    name: Option<String>,

    /// Where to send bytes to communicate to the other side
    outgoing_bytes: OB,

    /// Where to read bytes from the other side
    incoming_bytes: IB,

    /// Where the "outgoing messages" actor will receive messages.
    outgoing_rx: mpsc::UnboundedReceiver<OutgoingMessage>,

    /// Sender to send messages to the "outgoing message" actor.
    outgoing_tx: mpsc::UnboundedSender<OutgoingMessage>,

    /// Handler for incoming messages.
    handler: H,

    /// Receiver for new tasks.
    new_task_rx: mpsc::UnboundedReceiver<Task>,

    /// Sender to send messages to the "new task" actor.
    new_task_tx: mpsc::UnboundedSender<Task>,
}

impl<OB: AsyncWrite, IB: AsyncRead> JrConnection<OB, IB, NullHandler> {
    /// Create a new JrConnection that will read and write from the given streams.
    /// This type follows a builder pattern; use other methods to configure and then invoke
    /// [`Self:serve`] (to use as a server) or [`Self::with_client`] to use as a client.
    pub fn new(outgoing_bytes: OB, incoming_bytes: IB) -> Self {
        let (outgoing_tx, outgoing_rx) = mpsc::unbounded();
        let (new_task_tx, new_task_rx) = mpsc::unbounded();
        Self {
            name: None,
            outgoing_bytes,
            incoming_bytes,
            outgoing_rx,
            outgoing_tx,
            handler: NullHandler::default(),
            new_task_rx,
            new_task_tx,
        }
    }
}

impl<OB: AsyncWrite, IB: AsyncRead, H: JrHandler> JrConnection<OB, IB, H> {
    /// Set the "name" of this connection -- used only for debugging logs.
    pub fn name(mut self, name: impl ToString) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Add a new [`JrHandler`] to the chain.
    ///
    /// Prefer [`Self::on_receive_request`] or [`Self::on_receive_notification`].
    /// This is a low-level method that is not intended for general use.
    pub fn chain_handler<H1: JrHandler>(
        self,
        handler: H1,
    ) -> JrConnection<OB, IB, ChainHandler<H, H1>> {
        JrConnection {
            name: self.name,
            handler: ChainHandler::new(self.handler, handler),
            outgoing_bytes: self.outgoing_bytes,
            incoming_bytes: self.incoming_bytes,
            outgoing_rx: self.outgoing_rx,
            outgoing_tx: self.outgoing_tx,
            new_task_rx: self.new_task_rx,
            new_task_tx: self.new_task_tx,
        }
    }

    /// Register a handler for messages that can be either requests OR notifications.
    ///
    /// Use this when you want to handle an enum type that contains both request and
    /// notification variants. Your handler receives a [`MessageAndCx<R, N>`] which
    /// is an enum with two variants:
    ///
    /// - `MessageAndCx::Request(request, request_cx)` - A request with its response context
    /// - `MessageAndCx::Notification(notification, cx)` - A notification with the connection context
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Define an enum containing both requests and notifications
    /// enum MyMessages {
    ///     Request(MyRequests),
    ///     Notification(MyNotifications),
    /// }
    ///
    /// connection.on_receive_message(async |message: MessageAndCx<MyRequests, MyNotifications>| {
    ///     match message {
    ///         MessageAndCx::Request(req, request_cx) => {
    ///             // Handle request and send response
    ///             request_cx.respond(MyResponse { ... })
    ///         }
    ///         MessageAndCx::Notification(notif, _cx) => {
    ///             // Handle notification (no response needed)
    ///             Ok(())
    ///         }
    ///     }
    /// })
    /// ```
    ///
    /// For most use cases, prefer [`on_receive_request`](Self::on_receive_request) or
    /// [`on_receive_notification`](Self::on_receive_notification) which provide cleaner APIs
    /// for handling requests or notifications separately.
    pub fn on_receive_message<R, N, F>(
        self,
        op: F,
    ) -> JrConnection<OB, IB, ChainHandler<H, MessageHandler<R, N, F>>>
    where
        R: JsonRpcRequest,
        N: JrNotification,
        F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), crate::Error>,
    {
        JrConnection {
            name: self.name,
            handler: ChainHandler::new(self.handler, MessageHandler::new(op)),
            outgoing_bytes: self.outgoing_bytes,
            incoming_bytes: self.incoming_bytes,
            outgoing_rx: self.outgoing_rx,
            outgoing_tx: self.outgoing_tx,
            new_task_rx: self.new_task_rx,
            new_task_tx: self.new_task_tx,
        }
    }

    /// Register a handler for JSON-RPC requests of type `R`.
    ///
    /// Your handler receives two arguments:
    /// 1. The request (type `R`)
    /// 2. A [`JrRequestCx<R::Response>`] for sending the response
    ///
    /// The request context allows you to:
    /// - Send the response with [`JrRequestCx::respond`]
    /// - Send notifications to the client with [`JrConnectionCx::send_notification`]
    /// - Send requests to the client with [`JrConnectionCx::send_request`]
    ///
    /// # Example
    ///
    /// ```
    /// # use sacp::{JrConnection, PromptRequest, PromptResponse, SessionNotification};
    /// # fn example(connection: JrConnection<impl futures::AsyncWrite, impl futures::AsyncRead, impl sacp::JrHandler>) {
    /// connection.on_receive_request(async |request: PromptRequest, request_cx| {
    ///     // Send a notification while processing
    ///     let notif: SessionNotification = todo!();
    ///     request_cx.send_notification(notif)?;
    ///
    ///     // Do some work...
    ///     let result = todo!("process the prompt");
    ///
    ///     // Send the response
    ///     let response: PromptResponse = todo!();
    ///     request_cx.respond(response)
    /// });
    /// # }
    /// ```
    ///
    /// # Type Parameter
    ///
    /// `R` can be either a single request type or an enum of multiple request types.
    /// See the [type-driven dispatch](Self#type-driven-message-dispatch) section for details.
    pub fn on_receive_request<R, F>(
        self,
        op: F,
    ) -> JrConnection<OB, IB, ChainHandler<H, RequestHandler<R, F>>>
    where
        R: JsonRpcRequest,
        F: AsyncFnMut(R, JrRequestCx<R::Response>) -> Result<(), crate::Error>,
    {
        JrConnection {
            name: self.name,
            handler: ChainHandler::new(self.handler, RequestHandler::new(op)),
            outgoing_bytes: self.outgoing_bytes,
            incoming_bytes: self.incoming_bytes,
            outgoing_rx: self.outgoing_rx,
            outgoing_tx: self.outgoing_tx,
            new_task_rx: self.new_task_rx,
            new_task_tx: self.new_task_tx,
        }
    }

    /// Register a handler for JSON-RPC notifications of type `N`.
    ///
    /// Notifications are fire-and-forget messages that don't expect a response.
    /// Your handler receives:
    /// 1. The notification (type `N`)
    /// 2. A [`JrConnectionCx`] for sending messages to the other side
    ///
    /// Unlike request handlers, you cannot send a response (notifications don't have IDs),
    /// but you can still send your own requests and notifications using the context.
    ///
    /// # Example
    ///
    /// ```rust,ignore
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
    /// })
    /// ```
    ///
    /// # Type Parameter
    ///
    /// `N` can be either a single notification type or an enum of multiple notification types.
    /// See the [type-driven dispatch](Self#type-driven-message-dispatch) section for details.
    pub fn on_receive_notification<N, F>(
        self,
        op: F,
    ) -> JrConnection<OB, IB, ChainHandler<H, NotificationHandler<N, F>>>
    where
        N: JrNotification,
        F: AsyncFnMut(N, JrConnectionCx) -> Result<(), crate::Error>,
    {
        JrConnection {
            name: self.name,
            handler: ChainHandler::new(self.handler, NotificationHandler::new(op)),
            outgoing_bytes: self.outgoing_bytes,
            incoming_bytes: self.incoming_bytes,
            outgoing_rx: self.outgoing_rx,
            outgoing_tx: self.outgoing_tx,
            new_task_rx: self.new_task_rx,
            new_task_tx: self.new_task_tx,
        }
    }

    /// Returns a [`JsonRpcCx`] that allows you to send requests over the connection
    /// and receive responses.
    ///
    /// **Warning:** This method is provided for use during setup and construction.
    /// It returns a [`JrConnectionCx`] that you can use later to send requests
    /// or to spawn tasks. But if you do not invoke [`Self::serve`] or [`Self::with_client`],
    /// the server will not be running, and those messages will not be received.
    pub fn json_rpc_cx(&self) -> JrConnectionCx {
        JrConnectionCx::new(self.outgoing_tx.clone(), self.new_task_tx.clone())
    }

    /// With a task that will be spawned on the server's executor when it is active.
    #[track_caller]
    pub fn with_spawned(
        self,
        task: impl Future<Output = Result<(), crate::Error>> + Send + 'static,
    ) -> Self {
        let json_rpc_cx = self.json_rpc_cx();
        json_rpc_cx.spawn(task).expect("spawning failed");
        self
    }

    /// Run the connection in server mode, processing incoming messages until the connection closes.
    ///
    /// This drives the connection by continuously reading messages from the input stream
    /// and dispatching them to your registered handlers. The connection will run until:
    /// - The input stream closes (EOF)
    /// - An error occurs
    /// - One of your handlers returns an error
    ///
    /// Use this mode when you only need to respond to incoming messages and don't need
    /// to initiate your own requests. If you need to send requests to the other side,
    /// use [`with_client`](Self::with_client) instead.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// JrConnection::new(stdout, stdin)
    ///     .on_receive_request(async |req: MyRequest, cx| {
    ///         // Handle requests...
    ///         cx.respond(MyResponse { ... })
    ///     })
    ///     .serve()  // Run until connection closes
    ///     .await?;
    /// ```
    pub async fn serve(self) -> Result<(), crate::Error> {
        let (reply_tx, reply_rx) = mpsc::unbounded();
        let json_rpc_cx = JrConnectionCx::new(self.outgoing_tx, self.new_task_tx);
        futures::select!(
            r = actors::outgoing_actor(
                &self.name,
                self.outgoing_rx,
                reply_tx.clone(),
                self.outgoing_bytes,
            ).fuse() => {
                tracing::trace!(?r, "outgoing actor terminated");
                r?;
            }
            r = actors::incoming_actor(
                &self.name,
                &json_rpc_cx,
                self.incoming_bytes,
                reply_tx,
                self.handler,
            ).fuse() => {
                tracing::trace!(?r, "incoming actor terminated");
                r?;
            }
            r = actors::reply_actor(reply_rx).fuse() => {
                tracing::trace!(?r, "reply actor terminated");
                r?;
            }
            r = actors::task_actor(self.new_task_rx).fuse() => {
                tracing::trace!(?r, "task actor terminated");
                r?;
            }
        );
        Ok(())
    }

    /// Run the connection in client mode, both handling incoming messages and sending your own.
    ///
    /// This drives the connection by:
    /// 1. Running your registered handlers in the background to process incoming messages
    /// 2. Executing your `main_fn` closure with a [`JrConnectionCx`] for sending requests/notifications
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
    /// ```rust,ignore
    /// JrConnection::new(stdout, stdin)
    ///     .on_receive_request(async |req: MyRequest, cx| {
    ///         // Handle incoming requests in the background
    ///         cx.respond(MyResponse { ... })
    ///     })
    ///     .with_client(async |cx| {
    ///         // Initialize the protocol
    ///         let init_response = cx.send_request(InitializeRequest { ... })
    ///             .block_task()
    ///             .await?;
    ///
    ///         // Send more requests...
    ///         let result = cx.send_request(SomeRequest { ... })
    ///             .block_task()
    ///             .await?;
    ///
    ///         // When this closure returns, the connection shuts down
    ///         Ok(())
    ///     })
    ///     .await?;
    /// ```
    ///
    /// # Parameters
    ///
    /// - `main_fn`: Your client logic. Receives a [`JrConnectionCx`] for sending messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection closes before `main_fn` completes.
    pub async fn with_client(
        self,
        main_fn: impl AsyncFnOnce(JrConnectionCx) -> Result<(), crate::Error>,
    ) -> Result<(), crate::Error> {
        let cx = self.json_rpc_cx();

        // Run the server + the main function until one terminates.
        // We EXPECT the main function to be the one to terminate
        // except in case of error.
        let result = futures::future::select(Box::pin(self.serve()), Box::pin(main_fn(cx))).await;

        match result {
            Either::Left((result, _)) | Either::Right((result, _)) => result,
        }
    }
}

/// Message sent to the reply management actor
enum ReplyMessage {
    /// Wait for a response to the given id and then send it to the given receiver
    Subscribe(
        jsonrpcmsg::Id,
        oneshot::Sender<Result<serde_json::Value, crate::Error>>,
    ),

    /// Dispatch a response to the given id and value
    Dispatch(jsonrpcmsg::Id, Result<serde_json::Value, crate::Error>),
}

/// Messages send to be serialized over the transport.
#[derive(Debug)]
enum OutgoingMessage {
    /// Send a request to the server.
    Request {
        /// method to use in the request
        method: String,

        /// parameters for the request
        params: Option<jsonrpcmsg::Params>,

        /// where to send the response when it arrives
        response_tx: oneshot::Sender<Result<serde_json::Value, crate::Error>>,
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

/// Handlers are invoked when new messages arrive at the [`JrServer`].
/// They have a chance to inspect the method and parameters and decide whether to "claim" the request
/// (i.e., handle it). If they do not claim it, the request will be passed to the next handler.
#[allow(async_fn_in_trait)]
pub trait JrHandler {
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
    /// * `cx` - The context of the request. This gives access to the request ID and the method name and is used to send a reply; can also be used to send other messages to the other party.
    /// * `params` - The parameters of the request.
    ///
    /// # Returns
    ///
    /// * `Ok(Handled::Yes)` if the message was claimed. It will not be propagated further.
    /// * `Ok(Handled::No(message))` if not; the (possibly changed) message will be passed to the remaining handlers.
    /// * `Err` if an internal error occurs (this will bring down the server).
    async fn handle_message(
        &mut self,
        message: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, crate::Error>;

    fn describe_chain(&self) -> impl std::fmt::Debug;
}

/// Return type from JrHandler; indicates whether the request was handled or not.
#[must_use]
pub enum Handled<T> {
    Yes,
    No(T),
}

/// Connection context used to send requests/notifications of the other side.
#[derive(Clone, Debug)]
pub struct JrConnectionCx {
    message_tx: mpsc::UnboundedSender<OutgoingMessage>,
    task_tx: mpsc::UnboundedSender<Task>,
}

impl JrConnectionCx {
    fn new(
        tx: mpsc::UnboundedSender<OutgoingMessage>,
        task_tx: mpsc::UnboundedSender<Task>,
    ) -> Self {
        Self {
            message_tx: tx,
            task_tx,
        }
    }

    /// Spawns a task that will run so long as the JSON-RPC connection is being served.
    /// If the task returns an error, the server will shut down.
    #[track_caller]
    pub fn spawn(
        &self,
        future: impl Future<Output = Result<(), crate::Error>> + Send + 'static,
    ) -> Result<(), crate::Error> {
        self.task_tx
            .unbounded_send(Task {
                location: Location::caller(),
                future: Box::pin(future),
            })
            .map_err(crate::Error::into_internal_error)
    }

    /// Send a request/notification and forward the response appropriately.
    ///
    /// The request context's response type matches the request's response type,
    /// enabling type-safe message forwarding.
    pub fn send_proxied_message<R, N>(
        &self,
        message: MessageAndCx<R, N>,
    ) -> Result<(), crate::Error>
    where
        R: JsonRpcRequest<Response: Send>,
        N: JrNotification,
    {
        match message {
            MessageAndCx::Request(request, request_cx) => {
                self.send_request(request).forward_to_request_cx(request_cx)
            }
            MessageAndCx::Notification(notification, _) => self.send_notification(notification),
        }
    }

    /// Send an outgoing request and await the reply.
    pub fn send_request<Req: JsonRpcRequest>(&self, request: Req) -> JrResponse<Req::Response> {
        let method = request.method().to_string();
        let (response_tx, response_rx) = oneshot::channel();
        match request.into_untyped_message() {
            Ok(untyped) => {
                let params = crate::util::json_cast(untyped.params).ok();
                let message = OutgoingMessage::Request {
                    method: method.clone(),
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
                            .send(Err(communication_failure(format!(
                                "failed to send outgoing request `{method}"
                            ))))
                            .unwrap();
                    }
                }
            }

            Err(_) => {
                response_tx
                    .send(Err(communication_failure(format!(
                        "failed to send outgoing request `{method}"
                    ))))
                    .unwrap();
            }
        }

        JrResponse::new(method.clone(), self.clone(), response_rx)
            .map(move |json| <Req::Response>::from_value(&method, json))
    }

    /// Send an outgoing notification (no reply expected).)
    pub fn send_notification<N: JrNotification>(
        &self,
        notification: N,
    ) -> Result<(), crate::Error> {
        let untyped = notification.into_untyped_message()?;
        let params = crate::util::json_cast(untyped.params).ok();
        self.send_raw_message(OutgoingMessage::Notification {
            method: untyped.method,
            params,
        })
    }

    /// Send an error notification (no reply expected).
    pub fn send_error_notification(&self, error: crate::Error) -> Result<(), crate::Error> {
        self.send_raw_message(OutgoingMessage::Error { error })
    }

    fn send_raw_message(&self, message: OutgoingMessage) -> Result<(), crate::Error> {
        match &message {
            OutgoingMessage::Response { id, response } => match response {
                Ok(_) => tracing::debug!(?id, "send_raw_message: queuing success response"),
                Err(e) => tracing::warn!(?id, ?e, "send_raw_message: queuing error response"),
            },
            _ => {}
        }
        self.message_tx
            .unbounded_send(message)
            .map_err(communication_failure)
    }
}

/// The context to respond to an incoming request.
/// Derefs to a [`JsonRpcCx`] which can be used to send other requests and notification.
#[must_use]
pub struct JrRequestCx<T: JrResponsePayload> {
    /// The context to use to send outgoing messages and replies.
    cx: JrConnectionCx,

    /// The method of the request.
    method: String,

    /// The `id` of the message we are replying to.
    id: jsonrpcmsg::Id,

    /// Function to send the response `T` to a request with the given method and id.
    make_json: SendBoxFnOnce<
        'static,
        (String, Result<T, crate::Error>),
        Result<serde_json::Value, crate::Error>,
    >,
}

impl<T: JrResponsePayload> std::fmt::Debug for JrRequestCx<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JrRequestCx")
            .field("cx", &self.cx)
            .field("method", &self.method)
            .field("id", &self.id)
            .field("response_type", &std::any::type_name::<T>())
            .finish()
    }
}

impl<T: JrResponsePayload> std::ops::Deref for JrRequestCx<T> {
    type Target = JrConnectionCx;

    fn deref(&self) -> &Self::Target {
        &self.cx
    }
}

impl JrRequestCx<serde_json::Value> {
    /// Create a new method context.
    fn new(cx: &JrConnectionCx, method: String, id: jsonrpcmsg::Id) -> Self {
        Self {
            cx: cx.clone(),
            method,
            id,
            make_json: SendBoxFnOnce::new(move |_method, value| value),
        }
    }

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

    /// Convert to a `JrRequestCx` that expects a JSON value
    /// and which checks (dynamically) that the JSON value it receives
    /// can be converted to `T`.
    pub fn erase_to_json(self) -> JrRequestCx<serde_json::Value> {
        self.wrap_params(|method, value| T::from_value(&method, value?))
    }

    /// Return a new JrResponse that expects a response of type U and serializes it.
    pub fn wrap_method(self, method: String) -> JrRequestCx<T> {
        JrRequestCx {
            cx: self.cx,
            method,
            id: self.id,
            make_json: self.make_json,
        }
    }

    /// Return a new JrResponse that expects a response of type U and serializes it.
    ///
    /// `wrap_fn` will be invoked with the method name and the result of the wrapped function.
    pub fn wrap_params<U: JrResponsePayload>(
        self,
        wrap_fn: impl FnOnce(&str, Result<U, crate::Error>) -> Result<T, crate::Error> + Send + 'static,
    ) -> JrRequestCx<U> {
        JrRequestCx {
            cx: self.cx,
            method: self.method,
            id: self.id,
            make_json: SendBoxFnOnce::new(move |method: String, input: Result<U, crate::Error>| {
                let t_value = wrap_fn(&method, input);
                self.make_json.call(method, t_value)
            }),
        }
    }

    /// Get the underlying JSON RPC context.
    pub fn connection_cx(&self) -> JrConnectionCx {
        self.cx.clone()
    }

    /// Respond to the JSON-RPC request with either a value (`Ok`) or an error (`Err`).
    pub fn respond_with_result(
        self,
        response: Result<T, crate::Error>,
    ) -> Result<(), crate::Error> {
        tracing::debug!(id = ?self.id, "respond called");
        let json = self.make_json.call_tuple((self.method.clone(), response));
        self.cx.send_raw_message(OutgoingMessage::Response {
            id: self.id,
            response: json,
        })
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

/// Common bounds for any JSON-RPC message.
pub trait JrMessage: 'static + Debug + Sized {
    /// The parameters for the request.
    fn into_untyped_message(self) -> Result<UntypedMessage, crate::Error>;

    /// The method name for the request.
    fn method(&self) -> &str;

    /// Attempt to parse this type from a JSON-RPC request.
    ///
    /// Returns:
    /// - `None` if this type does not recognize the method name or recognizes it as a notification
    /// - `Some(Ok(value))` if the method is recognized as a request and deserialization succeeds
    /// - `Some(Err(error))` if the method is recognized as a request but deserialization fails
    fn parse_request(_method: &str, _params: &impl Serialize)
    -> Option<Result<Self, crate::Error>>;

    /// Attempt to parse this type from a JSON-RPC notification.
    ///
    /// Returns:
    /// - `None` if this type does not recognize the method name or recognizes it as a request
    /// - `Some(Ok(value))` if the method is recognized as a notification and deserialization succeeds
    /// - `Some(Err(error))` if the method is recognized as a notification but deserialization fails
    fn parse_notification(
        _method: &str,
        _params: &impl Serialize,
    ) -> Option<Result<Self, crate::Error>>;
}

/// Defines the "payload" of a successful response to a JSON-RPC request.
pub trait JrResponsePayload: 'static + Debug + Sized {
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
pub trait JrNotification: JrMessage {}

/// A struct that represents a request (JSON-RPC message expecting a response).
pub trait JsonRpcRequest: JrMessage {
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
pub enum MessageAndCx<R: JsonRpcRequest = UntypedMessage, N: JrMessage = UntypedMessage> {
    /// Incoming request and the context where the response should be sent.
    Request(R, JrRequestCx<R::Response>),

    /// Incoming notification.
    Notification(N, JrConnectionCx),
}

impl<R: JsonRpcRequest, N: JrMessage> MessageAndCx<R, N> {
    /// Map the request and notification types to new types.
    pub fn map<R1, N1>(
        self,
        map_request: impl FnOnce(R, JrRequestCx<R::Response>) -> (R1, JrRequestCx<R1::Response>),
        map_notification: impl FnOnce(N, JrConnectionCx) -> (N1, JrConnectionCx),
    ) -> MessageAndCx<R1, N1>
    where
        R1: JsonRpcRequest<Response: Send>,
        N1: JrMessage,
    {
        match self {
            MessageAndCx::Request(request, cx) => {
                let (new_request, new_cx) = map_request(request, cx);
                MessageAndCx::Request(new_request, new_cx)
            }
            MessageAndCx::Notification(notification, cx) => {
                let (new_notification, new_cx) = map_notification(notification, cx);
                MessageAndCx::Notification(new_notification, new_cx)
            }
        }
    }

    /// Respond to the message with an error.
    ///
    /// If this message is a request, this error becomes the reply to the request.
    ///
    /// If this message is a notification, the error is sent as a notification.
    pub fn respond_with_error(self, error: crate::Error) -> Result<(), crate::Error> {
        match self {
            MessageAndCx::Request(_, cx) => cx.respond_with_error(error),
            MessageAndCx::Notification(_, cx) => cx.send_error_notification(error),
        }
    }
}

impl MessageAndCx {
    /// Returns the method of the message (only available for UntypedMessage).
    pub fn method(&self) -> &str {
        match self {
            MessageAndCx::Request(msg, _) => &msg.method,
            MessageAndCx::Notification(msg, _) => &msg.method,
        }
    }

    /// Returns the message of the message (only available for UntypedMessage).
    pub fn message(&self) -> &UntypedMessage {
        match self {
            MessageAndCx::Request(msg, _) => msg,
            MessageAndCx::Notification(msg, _) => msg,
        }
    }
}

/// An incoming JSON message without any typing. Can be a request or a notification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UntypedMessage {
    pub method: String,
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

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn params(&self) -> &serde_json::Value {
        &self.params
    }

    pub fn into_parts(self) -> (String, serde_json::Value) {
        (self.method, self.params)
    }
}

impl JrMessage for UntypedMessage {
    fn method(&self) -> &str {
        &self.method
    }

    fn into_untyped_message(self) -> Result<UntypedMessage, crate::Error> {
        Ok(self)
    }

    fn parse_request(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        Some(UntypedMessage::new(method, params))
    }

    fn parse_notification(
        method: &str,
        params: &impl Serialize,
    ) -> Option<Result<Self, crate::Error>> {
        Some(UntypedMessage::new(method, params))
    }
}

impl JsonRpcRequest for UntypedMessage {
    type Response = serde_json::Value;
}

impl JrNotification for UntypedMessage {}

/// Represents a pending response of type `R` from an outgoing request.
pub struct JrResponse<R> {
    method: String,
    connection_cx: JrConnectionCx,
    response_rx: oneshot::Receiver<Result<serde_json::Value, crate::Error>>,
    to_result: Box<dyn Fn(serde_json::Value) -> Result<R, crate::Error> + Send>,
}

impl JrResponse<serde_json::Value> {
    fn new(
        method: String,
        connection_cx: JrConnectionCx,
        response_rx: oneshot::Receiver<Result<serde_json::Value, crate::Error>>,
    ) -> Self {
        Self {
            method,
            response_rx,
            connection_cx,
            to_result: Box::new(Ok),
        }
    }
}

impl<R: JrResponsePayload> JrResponse<R> {
    /// Create a new response that maps the result of the response to a new type.
    pub fn map<U>(
        self,
        map_fn: impl Fn(R) -> Result<U, crate::Error> + 'static + Send,
    ) -> JrResponse<U>
    where
        U: JrResponsePayload,
    {
        JrResponse {
            method: self.method,
            response_rx: self.response_rx,
            connection_cx: self.connection_cx,
            to_result: Box::new(move |value| map_fn((self.to_result)(value)?)),
        }
    }

    /// Schedule an async task that will forward the respond to `response_cx` when it arrives.
    /// Useful when proxying messages around.
    pub fn forward_to_request_cx(self, request_cx: JrRequestCx<R>) -> Result<(), crate::Error>
    where
        R: Send,
    {
        self.await_when_result_received(async move |result| request_cx.respond_with_result(result))
    }

    /// Block the current task until the response is received.
    ///
    /// This is useful in spawned tasks. It should *not* be used
    /// in callbacks like [`JrConnection::on_receive_message`]
    /// as that will prevent the event loop from running.
    ///
    /// In a callback setting, prefer [`Self::await_when_response_received`].
    pub async fn block_task(self) -> Result<R, crate::Error>
    where
        R: Send,
    {
        match self.response_rx.await {
            Ok(Ok(json_value)) => match (self.to_result)(json_value) {
                Ok(value) => Ok(value),
                Err(err) => Err(err),
            },
            Ok(Err(err)) => Err(err),
            Err(err) => Err(crate::util::internal_error(format!(
                "response to `{}` never received: {}",
                self.method, err
            ))),
        }
    }

    /// Schedule an async task to run when a successful response is received.
    /// If an error occurs, that error will be returned to `request_cx` as the result.
    #[track_caller]
    pub fn await_when_ok_response_received<F>(
        self,
        request_cx: JrRequestCx<R>,
        task: impl FnOnce(R, JrRequestCx<R>) -> F + 'static + Send,
    ) -> Result<(), crate::Error>
    where
        F: Future<Output = Result<(), crate::Error>> + 'static + Send,
        R: Send,
    {
        self.await_when_result_received(async move |result| match result {
            Ok(value) => task(value, request_cx).await,
            Err(err) => request_cx.respond_with_error(err),
        })
    }

    /// Schedule an async task to run when the response is received.
    ///
    /// It is intentionally not possible to block until the response is received
    /// because doing so can easily stall the event loop if done directly in the `on_receive` callback.
    ///
    /// If this task ultimately returns `Err`, the server will abort.
    #[track_caller]
    pub fn await_when_result_received<F>(
        self,
        task: impl FnOnce(Result<R, crate::Error>) -> F + 'static + Send,
    ) -> Result<(), crate::Error>
    where
        F: Future<Output = Result<(), crate::Error>> + 'static + Send,
        R: Send,
    {
        let connection_cx = self.connection_cx.clone();
        let block_task = self.block_task();
        connection_cx.spawn(async move { task(block_task.await).await })
    }
}

const COMMUNICATION_FAILURE: i32 = -32000;

fn communication_failure(err: impl ToString) -> crate::Error {
    crate::Error::new((COMMUNICATION_FAILURE, err.to_string()))
}
