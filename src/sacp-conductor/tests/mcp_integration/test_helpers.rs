//! Test helpers to reduce boilerplate in MCP integration tests

use futures::{SinkExt, StreamExt, channel::mpsc};
use sacp::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, SessionId,
    SessionNotification, TextContent,
};
use sacp::{JrConnectionCx, JrHandlerChain};
use sacp_conductor::conductor::Conductor;
use std::time::Duration;
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Helper to await a JSON-RPC response
pub async fn recv<R: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<R>,
) -> Result<R, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.await_when_result_received(async move |result| {
        tx.send(result).map_err(|_| sacp::Error::internal_error())
    })?;
    rx.await.map_err(|_| sacp::Error::internal_error())?
}

/// Helper to await a response with timeout
pub async fn recv_with_timeout<R: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<R>,
    timeout: Duration,
) -> Result<R, sacp::Error> {
    tokio::time::timeout(timeout, recv(response))
        .await
        .map_err(|_| sacp::Error::internal_error())?
}

pub fn conductor_command() -> Vec<String> {
    vec![
        "cargo".to_string(),
        "run".to_string(),
        "-p".to_string(),
        "sacp-conductor".to_string(),
        "--".to_string(),
    ]
}

/// A test session with initialized connection and session ID
pub struct TestSession {
    pub session_id: SessionId,
    pub editor_cx: JrConnectionCx,
}

impl TestSession {
    /// Send a prompt and await response with timeout
    pub async fn prompt(
        &self,
        text: impl Into<String>,
    ) -> Result<sacp::schema::PromptResponse, sacp::Error> {
        recv_with_timeout(
            self.editor_cx.send_request(PromptRequest {
                session_id: self.session_id.clone(),
                prompt: vec![ContentBlock::Text(TextContent {
                    annotations: None,
                    text: text.into(),
                    meta: None,
                })],
                meta: None,
            }),
            Duration::from_secs(10),
        )
        .await
    }
}

/// Builder for creating MCP integration tests
pub struct TestBuilder {
    components: Vec<sacp::DynComponent>,
}

impl TestBuilder {
    pub fn new() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    /// Add a component to the conductor chain
    pub fn with_component(mut self, component: sacp::DynComponent) -> Self {
        self.components.push(component);
        self
    }

    /// Run the test with the given test function
    pub async fn run<F, Fut>(self, test_fn: F) -> Result<Vec<String>, sacp::Error>
    where
        F: FnOnce(TestSession) -> Fut,
        Fut: std::future::Future<Output = Result<(), sacp::Error>>,
    {
        let (log_tx, mut log_rx) = mpsc::unbounded();

        // Set up editor <-> conductor communication
        let (editor, conductor) = duplex(8192);
        let (editor_in, editor_out) = tokio::io::split(editor);
        let (conductor_in, conductor_out) = tokio::io::split(conductor);

        let transport = sacp::ByteStreams::new(editor_out.compat_write(), editor_in.compat());

        let mut log_tx_clone = log_tx.clone();
        JrHandlerChain::new()
            .name("test-editor")
            .on_receive_notification(async move |notification: SessionNotification, _cx| {
                log_tx_clone
                    .send(format!("{:?}", notification))
                    .await
                    .map_err(|_| sacp::Error::internal_error())
            })
            .with_spawned(|_cx| async move {
                Conductor::new(
                    "test-conductor".to_string(),
                    self.components,
                    Some(conductor_command()),
                )
                .run(sacp::ByteStreams::new(
                    conductor_out.compat_write(),
                    conductor_in.compat(),
                ))
                .await
            })
            .with_client(transport, async |editor_cx| {
                // Initialize
                recv(editor_cx.send_request(InitializeRequest {
                    protocol_version: Default::default(),
                    client_capabilities: Default::default(),
                    meta: None,
                    client_info: None,
                }))
                .await?;

                // Create session
                let session = recv(editor_cx.send_request(NewSessionRequest {
                    cwd: Default::default(),
                    mcp_servers: vec![],
                    meta: None,
                }))
                .await?;

                // Run the test function
                test_fn(TestSession {
                    session_id: session.session_id,
                    editor_cx,
                })
                .await
            })
            .await?;

        // Collect notifications
        drop(log_tx);
        let mut notifications = Vec::new();
        while let Some(entry) = log_rx.next().await {
            notifications.push(entry);
        }

        Ok(notifications)
    }
}
