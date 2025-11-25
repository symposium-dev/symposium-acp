use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response, Sse},
    routing::post,
};
use futures::{SinkExt, StreamExt as _, channel::mpsc, future::Either, stream::Stream};
use futures_concurrency::future::FutureExt as _;
use futures_concurrency::stream::StreamExt as _;
use fxhash::FxHashMap;
use sacp::{BoxFuture, Channel, Component, jsonrpcmsg::Message};
use std::{pin::pin, sync::Arc};
use tokio::net::TcpListener;

use crate::conductor::{
    ConductorMessage,
    mcp_bridge::{McpBridgeConnection, McpBridgeConnectionActor},
};

/// Runs an HTTP listener for MCP bridge connections
pub async fn run_http_listener(
    tcp_listener: TcpListener,
    acp_url: String,
    session_id: sacp::schema::SessionId,
    mut conductor_tx: mpsc::Sender<ConductorMessage>,
) -> Result<(), sacp::Error> {
    let (to_mcp_client_tx, to_mcp_client_rx) = mpsc::channel(128);

    // When we send this message to the conductor,
    // it is going to go through a step or two and eventually
    // spawn the McpBridgeConnectionActor, which will ferry MCP requests
    // back and forth.
    conductor_tx
        .send(ConductorMessage::McpConnectionReceived {
            acp_url: acp_url,
            session_id: session_id.clone(),
            actor: McpBridgeConnectionActor::new(
                HttpMcpBridge::new(tcp_listener),
                conductor_tx.clone(),
                to_mcp_client_rx,
            ),
            connection: McpBridgeConnection::new(to_mcp_client_tx),
        })
        .await
        .map_err(|_| sacp::Error::internal_error())?;

    Ok(())
}

/// A component that receives HTTP requests/responses using the HTTP transport defined by the MCP protocol.
struct HttpMcpBridge {
    listener: tokio::net::TcpListener,
}

impl HttpMcpBridge {
    /// Creates a new HTTP-MCP bridge from an existing TCP listener.
    fn new(listener: tokio::net::TcpListener) -> Self {
        Self { listener }
    }
}

impl sacp::Component for HttpMcpBridge {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        let (channel, serve_self) = self.into_server();
        match futures::future::select(pin!(client.serve(channel)), serve_self).await {
            Either::Left((result, _)) => result,
            Either::Right((result, _)) => result,
        }
    }

    fn into_server(self) -> (Channel, BoxFuture<'static, Result<(), sacp::Error>>)
    where
        Self: Sized,
    {
        let (channel_a, channel_b) = Channel::duplex();
        (channel_a, Box::pin(run(self.listener, channel_b)))
    }
}

/// Error type that we use to respond to malformed HTTP requests.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct HttpError(#[from] sacp::Error);

impl From<axum::Error> for HttpError {
    fn from(error: axum::Error) -> Self {
        HttpError(sacp::util::internal_error(error))
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let message = format!("Error: {}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
    }
}

/// Run a webserver listening on `listener` for HTTP requests at `/`
/// and communicating those requests over `channel` to the JSON-RPC server.
async fn run(listener: TcpListener, channel: Channel) -> Result<(), sacp::Error> {
    let (registration_tx, registration_rx) = mpsc::unbounded();

    let state = BridgeState { registration_tx };

    // The way that the MCP protocol works is a bit "special".
    //
    // Clients *POST* messages to `/`. Those are submitted to the MCP server.
    // If the message is a REQUEST, then the client waits until it gets a reply.
    // It expects the server to close the connection after responding.
    //
    // Clients can also issue a *GET* request. This will result in a stream of messages.
    //
    // Non-reply messages can be send to any open stream (POST, GET, etc) but must be sent to
    // exactly one.
    //
    // There are provisions for "resuming" from a blocked point by tagging each message in the SSE stream
    // with an id, but we are not implementing that because I am lazy.
    async {
        let app = Router::new()
            .route("/", post(handle_post).get(handle_get))
            .with_state(Arc::new(state));

        axum::serve(listener, app)
            .await
            .map_err(sacp::util::internal_error)
    }
    .race(RunningServer::new().run(channel, registration_rx))
    .await
}

/// The state we pass to our POST/GET handlers.
struct BridgeState {
    /// Where to send registration messages.
    registration_tx: mpsc::UnboundedSender<RegistrationMessage>,
}

/// This is sent when we receive a POST or GET.
#[derive(Debug)]
struct RegistrationMessage {
    /// The message received from the client (`None` for GET requests)
    message: Option<sacp::jsonrpcmsg::Message>,

    /// Where to send messages so they will be relayed to the client
    outgoing_tx: mpsc::UnboundedSender<sacp::jsonrpcmsg::Message>,
}

/// Clone of `sacp::jsonrpcmsg::Id` since for unfathomable reasons that does not impl Hash
#[derive(Eq, PartialEq, PartialOrd, Ord, Hash, Debug, Clone)]
enum JsonRpcId {
    /// String identifier
    String(String),
    /// Numeric identifier
    Number(u64),
    /// Null identifier (for notifications)
    Null,
}

impl From<sacp::jsonrpcmsg::Id> for JsonRpcId {
    fn from(id: sacp::jsonrpcmsg::Id) -> Self {
        match id {
            sacp::jsonrpcmsg::Id::String(s) => JsonRpcId::String(s),
            sacp::jsonrpcmsg::Id::Number(n) => JsonRpcId::Number(n),
            sacp::jsonrpcmsg::Id::Null => JsonRpcId::Null,
        }
    }
}

struct RunningServer {
    waiting_sessions: FxHashMap<JsonRpcId, RegisteredSession>,
    general_sessions: Vec<RegisteredSession>,
}

impl RunningServer {
    fn new() -> Self {
        RunningServer {
            waiting_sessions: Default::default(),
            general_sessions: Default::default(),
        }
    }

    /// The main loop: listen for incoming "registrations"
    /// (POSTs, GETs from the HTTP client)
    /// and for outgoing JSON-RPC messages to be dispatched.
    ///
    /// # Parameters
    ///
    /// * `channel`: The channel to use for sending/receiving JSON-RPC messages.
    /// * `registration_rx`: The receiver for registration messages from HTTP clients.
    async fn run(
        mut self,
        mut channel: Channel,
        registration_rx: mpsc::UnboundedReceiver<RegistrationMessage>,
    ) -> Result<(), sacp::Error> {
        #[derive(Debug)]
        enum MultiplexMessage {
            Registration(RegistrationMessage),
            JsonRpc(Result<sacp::jsonrpcmsg::Message, sacp::Error>),
        }

        let mut merged_stream = registration_rx
            .map(MultiplexMessage::Registration)
            .merge(channel.rx.map(MultiplexMessage::JsonRpc));

        while let Some(message) = merged_stream.next().await {
            tracing::trace!(?message, "received message");
            match message {
                MultiplexMessage::Registration(message) => {
                    self.register(message, &mut channel.tx).await?
                }

                MultiplexMessage::JsonRpc(message) => {
                    self.dispatch_jsonrpc_message(message).await?
                }
            }
        }

        tracing::trace!("http connection terminating");

        Ok(())
    }

    /// Invoked when there is an incoming POST/GET request.
    /// Registers this session with the bridge and, if this was a POST message, sends the
    /// message over the channel to whomever is waiting for it.
    async fn register(
        &mut self,
        RegistrationMessage {
            message,
            outgoing_tx,
        }: RegistrationMessage,
        channel_tx: &mut mpsc::UnboundedSender<Result<sacp::jsonrpcmsg::Message, sacp::Error>>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!(?message, "register");

        // Extract the message-id if this is a request that needs a reply.
        let message_id = match &message {
            Some(Message::Request(request)) => match &request.id {
                Some(v) => Some(v.clone().into()),
                None => None,
            },
            _ => None,
        };
        tracing::debug!(?message_id);

        // Send the message over the channel to be processed.
        if let Some(message) = message {
            channel_tx
                .unbounded_send(Ok(message))
                .map_err(sacp::util::internal_error)?;
        }

        // Record the registered session to receive replies or other messages.
        let registered_session = RegisteredSession { outgoing_tx };
        if let Some(message_id) = message_id {
            self.waiting_sessions.insert(message_id, registered_session);
        } else {
            self.general_sessions.push(registered_session);
        }

        // Just for good hygiene, purge closed sessions.
        self.purge_closed_sessions();

        Ok(())
    }

    /// Invoked when there is an outgoing JSON-RPC message to send.
    /// Finds a suitable place to send it or drops it on the floor.
    ///
    /// FIXME: We could buffer, replay, etc.
    async fn dispatch_jsonrpc_message(
        &mut self,
        message: Result<sacp::jsonrpcmsg::Message, sacp::Error>,
    ) -> Result<(), sacp::Error> {
        let mut message = message.unwrap_or_else(|err| {
            sacp::jsonrpcmsg::Message::Response(sacp::jsonrpcmsg::Response::error(
                sacp::util::into_jsonrpc_error(err),
                None,
            ))
        });

        // Extract the id of the message we are replying to, if any
        let message_id = match &message {
            Message::Response(response) => match &response.id {
                Some(v) => Some(v.clone().into()),
                None => None,
            },
            Message::Request(_) => None,
        };

        // If there is a specific id, try to send the message to that sender.
        // This also removes them from the list of waiting sessions.
        if let Some(message_id) = message_id {
            if let Some(RegisteredSession { outgoing_tx }) =
                self.waiting_sessions.remove(&message_id)
            {
                match outgoing_tx.unbounded_send(message) {
                    // Successfully sent the message, return
                    Ok(()) => return Ok(()),

                    // If the sender died, just recover the message and send it to anyone.
                    Err(m) => {
                        // If that sender is dead, remove them from the list
                        // and recover the message.
                        assert!(m.is_disconnected());
                        message = m.into_inner();
                    }
                }
            }
        }

        // Try to find *somewhere* to send the message
        self.purge_closed_sessions();
        let all_sessions = self
            .general_sessions
            .iter_mut()
            .chain(self.waiting_sessions.values_mut());
        for session in all_sessions {
            match session.outgoing_tx.unbounded_send(message) {
                Ok(()) => {
                    // Successfully sent the message, restore the general session and return.
                    return Ok(());
                }

                Err(m) => {
                    // If that session is dead, try the next one.
                    assert!(m.is_disconnected());
                    message = m.into_inner();
                }
            }
        }

        // If we don't find anywhere to send the message, just discard it. :shrug:
        Ok(())
    }

    /// Purge sessions from the bridge state where the receiver is closed.
    /// This happens when the HTTP client disconnects.
    fn purge_closed_sessions(&mut self) {
        self.general_sessions
            .retain(|session| !session.outgoing_tx.is_closed());
        self.waiting_sessions
            .retain(|_, session| !session.outgoing_tx.is_closed());
    }
}

struct RegisteredSession {
    outgoing_tx: mpsc::UnboundedSender<sacp::jsonrpcmsg::Message>,
}

/// Accept a POST request carrying a JsonRpc message from an MCP client.
/// For requests (messages with id), we return an SSE stream.
/// For notifications/responses (messages without id), we return 202 Accepted.
async fn handle_post(
    State(state): State<Arc<BridgeState>>,
    body: String,
) -> Result<Response, HttpError> {
    // Parse incoming JSON-RPC message
    let message: sacp::jsonrpcmsg::Message =
        serde_json::from_str(&body).map_err(sacp::util::parse_error)?;

    // Check if this is a request (has an id) or a notification/response
    let is_request = matches!(&message, Message::Request(req) if req.id.is_some());

    if is_request {
        // For requests, return an SSE stream
        Ok(handle_request(state, message).await?.into_response())
    } else {
        // For notifications/responses, send to server and return 202 Accepted
        handle_notification(state, message).await?;
        Ok(StatusCode::ACCEPTED.into_response())
    }
}

/// Accept a GET request from an MCP client.
/// We begin a streaming response for server-initiated messages.
async fn handle_get(
    State(state): State<Arc<BridgeState>>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, HttpError>>>, HttpError> {
    // GET requests don't include a message, just open an SSE stream
    handle_request(state, None).await
}

/// Handle a request that expects an SSE stream response.
/// Used for POST requests with an id, and GET requests.
async fn handle_request(
    state: Arc<BridgeState>,
    message: impl Into<Option<sacp::jsonrpcmsg::Message>>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, HttpError>>>, HttpError> {
    let mut registration_tx = state.registration_tx.clone();
    let (messages_tx, mut messages_rx) = mpsc::unbounded();
    registration_tx
        .send(RegistrationMessage {
            message: message.into(),
            outgoing_tx: messages_tx,
        })
        .await
        .map_err(sacp::util::internal_error)?;

    // Stream responses from ACP side.
    //
    // In the case of a JSON-RPC request having been posted,
    // the tx handle will be dropped once the response has arrived.
    //
    // In the case of GET requests, we'll just keep sending
    // messages back so long as the client is connected.
    let stream = async_stream::stream! {
        while let Some(message) = messages_rx.next().await {
            match axum::response::sse::Event::default().json_data(message) {
                Ok(v) => yield Ok(v),
                Err(e) => yield Err(HttpError::from(e)),
            }
        }
    };

    Ok(Sse::new(stream))
}

/// Handle a notification or response - just send it to the server, no response expected.
async fn handle_notification(
    state: Arc<BridgeState>,
    message: sacp::jsonrpcmsg::Message,
) -> Result<(), HttpError> {
    let mut registration_tx = state.registration_tx.clone();
    // Create a dummy channel - we won't use it since notifications don't get responses
    let (messages_tx, _messages_rx) = mpsc::unbounded();
    registration_tx
        .send(RegistrationMessage {
            message: Some(message),
            outgoing_tx: messages_tx,
        })
        .await
        .map_err(sacp::util::internal_error)?;
    Ok(())
}
