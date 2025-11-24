use axum::{Router, extract::State, response::Sse, routing::post};
use futures::{channel::mpsc, future::Either, stream::Stream};
use sacp::{BoxFuture, Channel, Component, jsonrpcmsg};
use std::{pin::pin, sync::Arc};
use tokio::net::TcpListener;

pub struct HttpMcpBridge {
    listener: tokio::net::TcpListener,
}

impl HttpMcpBridge {
    pub async fn new() -> Result<Self, sacp::Error> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(sacp::util::internal_error)?;
        Ok(Self { listener })
    }

    pub fn port(&self) -> u16 {
        self.listener.local_addr().unwrap().port()
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

async fn run(listener: TcpListener, channel: Channel) -> Result<(), sacp::Error> {
    let (registration_tx, registration_rx) = mpsc::channel(128);

    let state = BridgeState { registration_tx };

    let app = Router::new()
        .route("/", post(handle_post))
        .with_state(Arc::new(state));

    axum::serve(listener, app)
        .await
        .map_err(sacp::util::internal_error)
}

struct BridgeState {
    /// Where to send registration messages.
    registration_tx: mpsc::Sender<RegistrationMessage>,
}

/// This is sent when we receive a POST or GET.
struct RegistrationMessage {
    /// The message received from the client (`None` for GET requests)
    message: Option<sacp::jsonrpcmsg::Message>,

    /// Where to send messages so they will be relayed to the client
    outgoing_tx: mpsc::Sender<sacp::jsonrpcmsg::Message>,
}

async fn multiplexing_actor(
    _channel: Channel,
    _register_rx: mpsc::UnboundedReceiver<Result<jsonrpcmsg::Message, sacp::Error>>,
) {
}

struct RegisteredSession {}

async fn handle_post(
    State(_state): State<Arc<BridgeState>>,
    body: String,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, sacp::Error>>>, sacp::Error> {
    // Parse incoming JSON-RPC message
    let _message: sacp::jsonrpcmsg::Message = serde_json::from_str(&body).map_err(sacp::util::parse_error)?;

    // Stream responses from ACP side
    let stream = async_stream::stream! {
        yield Ok(axum::response::sse::Event::default());
    };

    Ok(Sse::new(stream))
}
