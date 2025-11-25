//! Trace event types for the sequence diagram viewer.
//!
//! Events are serialized as newline-delimited JSON (`.jsons` files).
//! The viewer loads these files to render interactive sequence diagrams.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
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

/// Writer for trace events, appending JSON lines to a file.
///
/// This is designed to be cloned and shared across async tasks.
/// All writes are serialized through an internal mutex.
#[derive(Clone)]
pub struct TraceWriter {
    inner: Arc<TraceWriterInner>,
}

struct TraceWriterInner {
    writer: Mutex<BufWriter<File>>,
    start_time: Instant,
}

impl TraceWriter {
    /// Create a new trace writer that appends to the given file path.
    pub fn new(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())?;

        Ok(Self {
            inner: Arc::new(TraceWriterInner {
                writer: Mutex::new(BufWriter::new(file)),
                start_time: Instant::now(),
            }),
        })
    }

    /// Get the elapsed time since trace start, in seconds.
    pub fn elapsed(&self) -> f64 {
        self.inner.start_time.elapsed().as_secs_f64()
    }

    /// Write a trace event to the file.
    pub fn write_event(&self, event: &TraceEvent) {
        let mut writer = self.inner.writer.lock().unwrap();
        // Ignore write errors - tracing should not break the conductor
        let _ = serde_json::to_writer(&mut *writer, event);
        let _ = writer.write_all(b"\n");
        let _ = writer.flush();
    }

    /// Write a request event.
    pub fn request(
        &self,
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
        &self,
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
        &self,
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
        &self,
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
