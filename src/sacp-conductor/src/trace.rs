//! Trace event types for the sequence diagram viewer.
//!
//! Events are serialized as newline-delimited JSON (`.jsons` files).
//! The viewer loads these files to render interactive sequence diagrams.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

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
}
