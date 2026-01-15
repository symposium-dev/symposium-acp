//! Trace event types for the sequence diagram viewer.
//!
//! Events are serialized as newline-delimited JSON (`.jsons` files).
//! The viewer loads these files to render interactive sequence diagrams.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use sacp::jsonrpcmsg;
use serde::{Deserialize, Serialize};

/// A trace event representing message flow between components.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceEvent {
    /// A JSON-RPC request from one component to another.
    Request(RequestEvent),

    /// A JSON-RPC response to a prior request.
    Response(ResponseEvent),

    /// A JSON-RPC notification (no response expected).
    Notification(NotificationEvent),

    /// A tracing log line from a component.
    Trace(TraceLogEvent),
}

/// Protocol type for messages.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// Standard ACP protocol messages.
    Acp,
    /// MCP-over-ACP messages (agent calling proxy's MCP server).
    Mcp,
}

/// A JSON-RPC request from one component to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEvent {
    /// Monotonic timestamp (seconds since trace start).
    pub ts: f64,

    /// Protocol: ACP or MCP.
    pub protocol: Protocol,

    /// Source component (e.g., "client", "proxy:0", "proxy:1", "agent").
    pub from: String,

    /// Destination component.
    pub to: String,

    /// JSON-RPC request ID (for correlating with response).
    pub id: serde_json::Value,

    /// JSON-RPC method name.
    pub method: String,

    /// ACP session ID, if known (null before session/new completes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,

    /// Full request params.
    pub params: serde_json::Value,
}

/// A JSON-RPC response to a prior request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEvent {
    /// Monotonic timestamp (seconds since trace start).
    pub ts: f64,

    /// Source component (who sent the response).
    pub from: String,

    /// Destination component (who receives the response).
    pub to: String,

    /// JSON-RPC request ID this responds to.
    pub id: serde_json::Value,

    /// True if this is an error response.
    pub is_error: bool,

    /// Response result or error object.
    pub payload: serde_json::Value,
}

/// A JSON-RPC notification (no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationEvent {
    /// Monotonic timestamp (seconds since trace start).
    pub ts: f64,

    /// Protocol: ACP or MCP.
    pub protocol: Protocol,

    /// Source component.
    pub from: String,

    /// Destination component.
    pub to: String,

    /// JSON-RPC method name.
    pub method: String,

    /// ACP session ID, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,

    /// Full notification params.
    pub params: serde_json::Value,
}

/// A tracing log line from a component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceLogEvent {
    /// Monotonic timestamp (seconds since trace start).
    pub ts: f64,

    /// Which component emitted this log.
    pub component: String,

    /// Log level.
    pub level: TraceLevel,

    /// Log message.
    pub message: String,

    /// Optional structured fields from tracing spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Log level for trace events.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Trait for destinations that can receive trace events.
pub trait WriteEvent: Send + 'static {
    /// Write a trace event to the destination.
    fn write_event(&mut self, event: &TraceEvent) -> std::io::Result<()>;
}

/// Writes trace events as newline-delimited JSON to a `Write` impl.
pub struct EventWriter<W> {
    writer: W,
}

impl<W: Write> EventWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: Write + Send + 'static> WriteEvent for EventWriter<W> {
    fn write_event(&mut self, event: &TraceEvent) -> std::io::Result<()> {
        serde_json::to_writer(&mut self.writer, event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }
}

/// Impl for UnboundedSender - sends events to a channel (useful for testing).
impl WriteEvent for futures::channel::mpsc::UnboundedSender<TraceEvent> {
    fn write_event(&mut self, event: &TraceEvent) -> std::io::Result<()> {
        self.unbounded_send(event.clone())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    }
}

/// Writer for trace events.
pub struct TraceWriter {
    dest: Box<dyn WriteEvent>,
    start_time: Instant,
}

impl TraceWriter {
    /// Create a new trace writer from any WriteEvent destination.
    pub fn new<D: WriteEvent>(dest: D) -> Self {
        Self {
            dest: Box::new(dest),
            start_time: Instant::now(),
        }
    }

    /// Create a new trace writer that writes to a file path.
    pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path.as_ref())?;
        Ok(Self::new(EventWriter::new(BufWriter::new(file))))
    }

    /// Get the elapsed time since trace start, in seconds.
    pub fn elapsed(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Write a trace event.
    pub fn write_event(&mut self, event: &TraceEvent) {
        // Ignore errors - tracing should not break the conductor
        let _ = self.dest.write_event(event);
    }

    /// Write a request event.
    pub fn request(
        &mut self,
        protocol: Protocol,
        from: impl Into<String>,
        to: impl Into<String>,
        id: serde_json::Value,
        method: impl Into<String>,
        session: Option<String>,
        params: serde_json::Value,
    ) {
        self.write_event(&TraceEvent::Request(RequestEvent {
            ts: self.elapsed(),
            protocol,
            from: from.into(),
            to: to.into(),
            id,
            method: method.into(),
            session,
            params,
        }));
    }

    /// Write a response event.
    pub fn response(
        &mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        id: serde_json::Value,
        is_error: bool,
        payload: serde_json::Value,
    ) {
        self.write_event(&TraceEvent::Response(ResponseEvent {
            ts: self.elapsed(),
            from: from.into(),
            to: to.into(),
            id,
            is_error,
            payload,
        }));
    }

    /// Write a notification event.
    pub fn notification(
        &mut self,
        protocol: Protocol,
        from: impl Into<String>,
        to: impl Into<String>,
        method: impl Into<String>,
        session: Option<String>,
        params: serde_json::Value,
    ) {
        self.write_event(&TraceEvent::Notification(NotificationEvent {
            ts: self.elapsed(),
            protocol,
            from: from.into(),
            to: to.into(),
            method: method.into(),
            session,
            params,
        }));
    }

    /// Write a trace log event.
    pub fn trace_log(
        &mut self,
        component: impl Into<String>,
        level: TraceLevel,
        message: impl Into<String>,
        fields: Option<serde_json::Map<String, serde_json::Value>>,
    ) {
        self.write_event(&TraceEvent::Trace(TraceLogEvent {
            ts: self.elapsed(),
            component: component.into(),
            level,
            message: message.into(),
            fields,
        }));
    }

    /// Trace a raw JSON-RPC message being sent from one component to another.
    pub fn trace_message(&mut self, from: &str, to: &str, message: &jsonrpcmsg::Message) {
        match message {
            jsonrpcmsg::Message::Request(req) => {
                let is_notification = req.id.is_none();
                let (protocol, method, params) = extract_message_info(&req.method, &req.params);

                if is_notification {
                    self.notification(protocol, from, to, method, None, params);
                } else {
                    let id = req
                        .id
                        .as_ref()
                        .map(id_to_json)
                        .unwrap_or(serde_json::Value::Null);
                    self.request(protocol, from, to, id, method, None, params);
                }
            }
            jsonrpcmsg::Message::Response(resp) => {
                let id = resp
                    .id
                    .as_ref()
                    .map(id_to_json)
                    .unwrap_or(serde_json::Value::Null);
                let (is_error, payload) = match (&resp.result, &resp.error) {
                    (Some(result), _) => (false, result.clone()),
                    (_, Some(error)) => (true, serde_json::to_value(error).unwrap_or_default()),
                    (None, None) => (false, serde_json::Value::Null),
                };
                self.response(from, to, id, is_error, payload);
            }
        }
    }
}

/// A cloneable handle for sending trace events to the trace writer task.
///
/// Create with [`spawn_trace_writer`], then clone and pass to bridges.
#[derive(Clone)]
pub struct TraceHandle {
    tx: futures::channel::mpsc::UnboundedSender<TraceEvent>,
    start_time: Instant,
}

impl TraceHandle {
    /// Send a trace event.
    pub fn send(&self, event: TraceEvent) {
        let _ = self.tx.unbounded_send(event);
    }

    /// Get the elapsed time since trace start, in seconds.
    pub fn elapsed(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Trace a raw JSON-RPC message being sent from one component to another.
    pub fn trace_message(&self, from: &str, to: &str, message: &jsonrpcmsg::Message) {
        let ts = self.elapsed();
        let event = match message {
            jsonrpcmsg::Message::Request(req) => {
                let is_notification = req.id.is_none();
                let (protocol, method, params) = extract_message_info(&req.method, &req.params);

                if is_notification {
                    TraceEvent::Notification(NotificationEvent {
                        ts,
                        protocol,
                        from: from.to_string(),
                        to: to.to_string(),
                        method,
                        session: None,
                        params,
                    })
                } else {
                    let id = req
                        .id
                        .as_ref()
                        .map(id_to_json)
                        .unwrap_or(serde_json::Value::Null);
                    TraceEvent::Request(RequestEvent {
                        ts,
                        protocol,
                        from: from.to_string(),
                        to: to.to_string(),
                        id,
                        method,
                        session: None,
                        params,
                    })
                }
            }
            jsonrpcmsg::Message::Response(resp) => {
                let id = resp
                    .id
                    .as_ref()
                    .map(id_to_json)
                    .unwrap_or(serde_json::Value::Null);
                let (is_error, payload) = match (&resp.result, &resp.error) {
                    (Some(result), _) => (false, result.clone()),
                    (_, Some(error)) => (true, serde_json::to_value(error).unwrap_or_default()),
                    (None, None) => (false, serde_json::Value::Null),
                };
                TraceEvent::Response(ResponseEvent {
                    ts,
                    from: from.to_string(),
                    to: to.to_string(),
                    id,
                    is_error,
                    payload,
                })
            }
        };
        self.send(event);
    }

    /// Create a tracing bridge that wraps a component.
    ///
    /// Returns a `DynComponent` that can be connected to, and a future that must be spawned.
    /// The bridge forwards messages between the channel and the component while tracing them.
    ///
    /// Tracing strategy:
    /// - **Left→Right (incoming)**: Trace requests/notifications, skip responses
    /// - **Right→Left (outgoing)**: Trace responses, and if `trace_outgoing_requests` is true,
    ///   also trace requests/notifications (needed for edge bridges at conductor boundaries)
    ///
    /// - `left_name`: Logical name of the component on the "left" side (e.g., "client", "proxy:0")
    /// - `right_name`: Logical name of the component on the "right" side (e.g., "proxy:0", "agent")
    /// - `component`: The component to wrap
    /// - `trace_outgoing_requests`: If true, also trace outgoing requests/notifications (for edge bridges)
    pub fn bridge<L: sacp::JrLink>(
        &self,
        left_name: String,
        right_name: String,
        component: impl sacp::Component<L> + 'static,
        trace_outgoing_requests: bool,
    ) -> (
        sacp::DynComponent<L>,
        futures::future::BoxFuture<'static, Result<(), sacp::Error>>,
    ) {
        use futures::StreamExt;

        let (channel_a, channel_b) = sacp::Channel::duplex();
        let (component_channel, component_future) = component.into_server();

        let trace_handle = self.clone();
        let left_name_clone = left_name.clone();
        let right_name_clone = right_name.clone();

        let bridge_future = Box::pin(async move {
            // Forward messages left→right (incoming to component)
            // Trace requests/notifications, skip responses
            // Also skip SuccessorMessages - they came from the right and were already traced
            let left_to_right = {
                let trace_handle = trace_handle.clone();
                let left_name = left_name.clone();
                let right_name = right_name.clone();
                let mut rx = channel_b.rx;
                let tx = component_channel.tx.clone();
                async move {
                    while let Some(msg) = rx.next().await {
                        if let Ok(ref m) = msg {
                            // Trace incoming requests/notifications, skip responses
                            // Also skip SuccessorMessages - they originate from the right
                            // (e.g., agent-to-client notifications wrapped as SuccessorMessage)
                            let should_trace = match m {
                                jsonrpcmsg::Message::Response(_) => false,
                                jsonrpcmsg::Message::Request(req) => {
                                    !is_successor_message(&req.method)
                                }
                            };
                            if should_trace {
                                trace_handle.trace_message(&left_name, &right_name, m);
                            }
                        }
                        if tx.unbounded_send(msg).is_err() {
                            break;
                        }
                    }
                    Ok::<_, sacp::Error>(())
                }
            };

            // Forward messages right→left (outgoing from component)
            // Trace responses always; trace requests/notifications only if trace_outgoing_requests
            // BUT: SuccessorMessage wrappers indicate the message is going RIGHT (to successor),
            // so we should NOT trace those here (they'll be traced as incoming at the next bridge)
            let right_to_left = {
                let trace_handle = trace_handle.clone();
                let mut rx = component_channel.rx;
                let tx = channel_b.tx;
                async move {
                    while let Some(msg) = rx.next().await {
                        if let Ok(ref m) = msg {
                            let should_trace = match m {
                                jsonrpcmsg::Message::Response(_) => true,
                                jsonrpcmsg::Message::Request(req) => {
                                    // SuccessorMessages are going RIGHT, not left
                                    // Don't trace them here - they'll be traced at the next bridge
                                    if is_successor_message(&req.method) {
                                        false
                                    } else {
                                        trace_outgoing_requests
                                    }
                                }
                            };
                            if should_trace {
                                trace_handle.trace_message(&right_name_clone, &left_name_clone, m);
                            }
                        }
                        if tx.unbounded_send(msg).is_err() {
                            break;
                        }
                    }
                    Ok::<_, sacp::Error>(())
                }
            };

            futures::try_join!(component_future, left_to_right, right_to_left)?;
            Ok(())
        });

        (sacp::DynComponent::new(channel_a), bridge_future)
    }
}

/// Spawn a trace writer task.
///
/// Returns a `TraceHandle` that can be cloned and used from multiple tasks,
/// and a future that should be spawned (e.g., via `with_spawned`).
pub fn spawn_trace_writer(
    mut writer: TraceWriter,
) -> (
    TraceHandle,
    impl std::future::Future<Output = Result<(), sacp::Error>>,
) {
    use futures::StreamExt;

    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let start_time = writer.start_time;

    let future = async move {
        while let Some(event) = rx.next().await {
            writer.write_event(&event);
        }
        Ok(())
    };

    (TraceHandle { tx, start_time }, future)
}

/// Check if a method is a SuccessorMessage wrapper.
fn is_successor_message(method: &str) -> bool {
    method == "_proxy/successor"
}

/// Convert a jsonrpcmsg::Id to serde_json::Value.
fn id_to_json(id: &jsonrpcmsg::Id) -> serde_json::Value {
    match id {
        jsonrpcmsg::Id::String(s) => serde_json::Value::String(s.clone()),
        jsonrpcmsg::Id::Number(n) => serde_json::Value::Number((*n).into()),
        jsonrpcmsg::Id::Null => serde_json::Value::Null,
    }
}

/// Extract logical message info from method and params.
///
/// This unwraps protocol wrappers to get the "real" message:
/// - `_proxy/successor` messages are unwrapped to get the inner message
/// - `_proxy/initialize` messages are unwrapped to get `initialize`
/// - `_mcp/message` messages are detected and marked as MCP protocol
///
/// Returns (protocol, method, params).
fn extract_message_info(
    method: &str,
    params: &Option<jsonrpcmsg::Params>,
) -> (Protocol, String, serde_json::Value) {
    let params_value = params
        .as_ref()
        .map(|p| match p {
            jsonrpcmsg::Params::Array(arr) => serde_json::Value::Array(arr.clone()),
            jsonrpcmsg::Params::Object(obj) => serde_json::Value::Object(obj.clone()),
        })
        .unwrap_or(serde_json::Value::Null);

    // Check for SuccessorMessage wrapper (_proxy/successor)
    const METHOD_SUCCESSOR_MESSAGE: &str = "_proxy/successor";
    if method == METHOD_SUCCESSOR_MESSAGE {
        // Extract the inner message from the flattened structure
        if let Some(inner_method) = params_value.get("method").and_then(|v| v.as_str()) {
            let inner_params = params_value
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            // Recursively extract in case of nested wrappers
            return extract_message_info_from_values(inner_method, &inner_params);
        }
    }

    // Check for InitializeProxyRequest wrapper (_proxy/initialize)
    // This wraps a standard `initialize` request
    const METHOD_INITIALIZE_PROXY: &str = "_proxy/initialize";
    if method == METHOD_INITIALIZE_PROXY {
        // The inner initialize params are flattened into the outer params
        return (Protocol::Acp, "initialize".to_string(), params_value);
    }

    // Check for MCP-over-ACP wrapper (_mcp/message)
    const METHOD_MCP_MESSAGE: &str = "_mcp/message";
    if method == METHOD_MCP_MESSAGE {
        // Extract the inner MCP message
        if let Some(inner_method) = params_value.get("method").and_then(|v| v.as_str()) {
            let inner_params = params_value
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            return (Protocol::Mcp, inner_method.to_string(), inner_params);
        }
    }

    // Regular ACP message
    (Protocol::Acp, method.to_string(), params_value)
}

/// Helper for recursive extraction from already-parsed values.
fn extract_message_info_from_values(
    method: &str,
    params: &serde_json::Value,
) -> (Protocol, String, serde_json::Value) {
    // Check for SuccessorMessage wrapper
    const METHOD_SUCCESSOR_MESSAGE: &str = "_proxy/successor";
    if method == METHOD_SUCCESSOR_MESSAGE {
        if let Some(inner_method) = params.get("method").and_then(|v| v.as_str()) {
            let inner_params = params
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            return extract_message_info_from_values(inner_method, &inner_params);
        }
    }

    // Check for MCP-over-ACP wrapper
    const METHOD_MCP_MESSAGE: &str = "_mcp/message";
    if method == METHOD_MCP_MESSAGE {
        if let Some(inner_method) = params.get("method").and_then(|v| v.as_str()) {
            let inner_params = params
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            return (Protocol::Mcp, inner_method.to_string(), inner_params);
        }
    }

    // Regular message
    (Protocol::Acp, method.to_string(), params.clone())
}
