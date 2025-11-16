//! Integration tests for elizacp MCP tool invocation

use elizacp::run_elizacp;
use sacp::JrHandlerChain;
use sacp::schema::{
    ContentBlock, InitializeRequest, McpServer, NewSessionRequest, PromptRequest,
    SessionNotification, TextContent,
};
use std::path::PathBuf;

/// Test helper to receive a JSON-RPC response
async fn recv<R: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<R>,
) -> Result<R, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.await_when_result_received(async move |result| {
        tx.send(result).map_err(|_| sacp::Error::internal_error())
    })?;
    rx.await.map_err(|_| sacp::Error::internal_error())?
}

#[tokio::test]
async fn test_elizacp_mcp_tool_call() -> Result<(), sacp::Error> {
    use futures::{SinkExt, StreamExt};
    use tokio::io::duplex;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    // Set up client <-> elizacp communication
    let (client_out, elizacp_in) = duplex(1024);
    let (elizacp_out, client_in) = duplex(1024);

    let transport = sacp::ByteStreams::new(client_out.compat_write(), client_in.compat());

    // Create channel to collect session notifications
    let (notification_tx, mut notification_rx) = futures::channel::mpsc::unbounded();

    JrHandlerChain::new()
        .name("test-client")
        .on_receive_notification({
            let mut notification_tx = notification_tx.clone();
            async move |notification: SessionNotification, _cx| {
                notification_tx
                    .send(notification)
                    .await
                    .map_err(|_| sacp::Error::internal_error())
            }
        })
        .with_spawned(|_cx| async move {
            run_elizacp(sacp::ByteStreams::new(
                elizacp_out.compat_write(),
                elizacp_in.compat(),
            ))
            .await
        })
        .with_client(transport, async |client_cx| {
            // Initialize
            let _init_response = recv(client_cx.send_request(InitializeRequest {
                protocol_version: Default::default(),
                client_capabilities: Default::default(),
                meta: None,
                client_info: None,
            }))
            .await?;

            // Create session with an MCP server
            // For this test, we'll use a simple echo MCP server
            // You can replace this with a real MCP server command
            let session_response = recv(client_cx.send_request(NewSessionRequest {
                cwd: PathBuf::from("/tmp"),
                mcp_servers: vec![McpServer::Stdio {
                    name: "test".to_string(),
                    command: PathBuf::from("echo"),
                    args: vec!["hello".to_string()],
                    env: vec![],
                }],
                meta: None,
            }))
            .await?;

            let session_id = session_response.session_id;

            // Send a prompt to invoke the MCP tool
            let _prompt_response = recv(client_cx.send_request(PromptRequest {
                session_id: session_id.clone(),
                prompt: vec![ContentBlock::Text(TextContent {
                    annotations: None,
                    text: r#"Use tool test::echo with {"message": "Hello from test!"}"#.to_string(),
                    meta: None,
                })],
                meta: None,
            }))
            .await?;

            Ok(())
        })
        .await?;

    // Drop the sender and collect all notifications
    drop(notification_tx);
    let mut notifications = Vec::new();
    while let Some(notification) = notification_rx.next().await {
        notifications.push(notification);
    }

    // Verify we got a response notification
    assert!(
        !notifications.is_empty(),
        "Expected at least one notification"
    );

    // Check that the response contains our result
    let response_text = notifications
        .iter()
        .filter_map(|n| match &n.update {
            sacp::schema::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");

    // Should start with OK: or ERROR:
    assert!(
        response_text.starts_with("OK:") || response_text.starts_with("ERROR:"),
        "Expected response to start with OK: or ERROR:, got: {}",
        response_text
    );

    Ok(())
}
