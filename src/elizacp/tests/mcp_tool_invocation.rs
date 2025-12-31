//! Integration tests for elizacp MCP tool invocation

use elizacp::ElizaAgent;
use expect_test::expect;
use sacp::Component;
use sacp::link::UntypedLink;
use sacp::schema::{
    ContentBlock, InitializeRequest, McpServer, McpServerStdio, NewSessionRequest, PromptRequest,
    ProtocolVersion, SessionNotification, TextContent,
};
use sacp_test::test_binaries;
use std::path::PathBuf;

/// Test helper to receive a JSON-RPC response
async fn recv<T: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<T>,
) -> Result<T, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.on_receiving_result(async move |result| {
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

    UntypedLink::builder()
        .name("test-client")
        .on_receive_notification(
            {
                let mut notification_tx = notification_tx.clone();
                async move |notification: SessionNotification, _cx| {
                    notification_tx
                        .send(notification)
                        .await
                        .map_err(|_| sacp::Error::internal_error())
                }
            },
            sacp::on_receive_notification!(),
        )
        .with_spawned(|_cx| async move {
            ElizaAgent::new(true)
                .serve(sacp::ByteStreams::new(
                    elizacp_out.compat_write(),
                    elizacp_in.compat(),
                ))
                .await
        })
        .run_until(transport, async |client_cx| {
            // Initialize
            let _init_response =
                recv(client_cx.send_request(InitializeRequest::new(ProtocolVersion::LATEST)))
                    .await?;

            // Create session with an MCP server
            // Use the mcp-echo-server from sacp-test (pre-built binary)
            let mcp_server_binary = test_binaries::mcp_echo_server_binary();
            let session_response = recv(client_cx.send_request(
                NewSessionRequest::new(PathBuf::from("/tmp")).mcp_servers(vec![McpServer::Stdio(
                    McpServerStdio::new("test".to_string(), mcp_server_binary),
                )]),
            ))
            .await?;

            let session_id = session_response.session_id;

            // Send a prompt to invoke the MCP tool
            let _prompt_response = recv(client_cx.send_request(PromptRequest::new(
                session_id.clone(),
                vec![ContentBlock::Text(TextContent::new(
                    r#"Use tool test::echo with {"message": "Hello from test!"}"#.to_string(),
                ))],
            )))
            .await?;

            Ok(())
        })
        .await?;

    // Drop the sender and collect all notifications
    drop(notification_tx);
    let mut notification_texts = Vec::new();
    while let Some(notification) = notification_rx.next().await {
        // Extract just the text content from notifications, ignoring session IDs
        if let sacp::schema::SessionUpdate::AgentMessageChunk(chunk) = notification.update {
            if let ContentBlock::Text(text) = chunk.content {
                notification_texts.push(text.text);
            }
        }
    }

    // Verify the output with expect_test
    // Should get a successful response from the echo tool
    expect![[r#"
        [
            "OK: CallToolResult { content: [Annotated { raw: Text(RawTextContent { text: \"Echo: Hello from test!\", meta: None }), annotations: None }], structured_content: None, is_error: Some(false), meta: None }",
        ]
    "#]]
    .assert_debug_eq(&notification_texts);

    Ok(())
}
