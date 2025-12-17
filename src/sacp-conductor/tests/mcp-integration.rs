//! Integration tests for MCP tool routing through proxy components.
//!
//! These tests verify that:
//! 1. Proxy components can provide MCP tools
//! 2. Agent components can discover and invoke those tools
//! 3. Tool invocations route correctly through the proxy

mod mcp_integration;

use elizacp::ElizaAgent;
use futures::{SinkExt, StreamExt, channel::mpsc};
use sacp::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, SessionNotification,
    TextContent,
};
use sacp_conductor::Conductor;
use sacp_conductor::McpBridgeMode;
use sacp_test::test_binaries;

use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Test helper to receive a JSON-RPC response
async fn recv<T: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<T>,
) -> Result<T, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.await_when_result_received(async move |result| {
        tx.send(result).map_err(|_| sacp::Error::internal_error())
    })?;
    rx.await.map_err(|_| sacp::Error::internal_error())?
}

fn conductor_command() -> Vec<String> {
    let binary_path = test_binaries::conductor_binary();
    vec![binary_path.to_string_lossy().to_string()]
}

async fn run_test_with_mode(
    mode: McpBridgeMode,
    components: Vec<sacp::DynComponent>,
    editor_task: impl AsyncFnOnce(sacp::JrConnectionCx<sacp::ClientToAgent>) -> Result<(), sacp::Error>,
) -> Result<(), sacp::Error> {
    // Initialize tracing for debug output
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    // Set up editor <-> conductor communication
    let (editor_out, conductor_in) = duplex(1024);
    let (conductor_out, editor_in) = duplex(1024);

    let transport = sacp::ByteStreams::new(editor_out.compat_write(), editor_in.compat());

    sacp::ClientToAgent::builder()
        .name("editor-to-connector")
        .with_spawned(|_cx| async move {
            Conductor::new("conductor".to_string(), components, mode)
                .run(sacp::ByteStreams::new(
                    conductor_out.compat_write(),
                    conductor_in.compat(),
                ))
                .await
        })
        .with_client(transport, editor_task)
        .await
}

/// Test that proxy-provided MCP tools work with stdio bridge mode
#[tokio::test]
async fn test_proxy_provides_mcp_tools_stdio() -> Result<(), sacp::Error> {
    run_test_with_mode(
        McpBridgeMode::Stdio {
            conductor_command: conductor_command(),
        },
        vec![
            mcp_integration::proxy::create(),
            sacp::DynComponent::new(ElizaAgent::new()),
        ],
        async |editor_cx| {
            // Send initialization request
            let init_response = recv(editor_cx.send_request(InitializeRequest {
                protocol_version: Default::default(),
                client_capabilities: Default::default(),
                meta: None,
                client_info: None,
            }))
            .await;

            assert!(
                init_response.is_ok(),
                "Initialize should succeed: {:?}",
                init_response
            );

            // Send session/new request
            let session_response = recv(editor_cx.send_request(NewSessionRequest {
                cwd: Default::default(),
                mcp_servers: vec![],
                meta: None,
            }))
            .await;

            assert!(
                session_response.is_ok(),
                "Session/new should succeed: {:?}",
                session_response
            );

            let session = session_response.unwrap();
            // ElizACP generates UUID session IDs, just verify it's non-empty
            assert!(!session.session_id.0.is_empty());

            Ok(())
        },
    )
    .await?;

    Ok(())
}

/// Test that proxy-provided MCP tools work with HTTP bridge mode
#[tokio::test]
async fn test_proxy_provides_mcp_tools_http() -> Result<(), sacp::Error> {
    run_test_with_mode(
        McpBridgeMode::Http,
        vec![
            mcp_integration::proxy::create(),
            sacp::DynComponent::new(ElizaAgent::new()),
        ],
        async |editor_cx| {
            // Send initialization request
            let init_response = recv(editor_cx.send_request(InitializeRequest {
                protocol_version: Default::default(),
                client_capabilities: Default::default(),
                meta: None,
                client_info: None,
            }))
            .await;

            assert!(
                init_response.is_ok(),
                "Initialize should succeed: {:?}",
                init_response
            );

            // Send session/new request
            let session_response = recv(editor_cx.send_request(NewSessionRequest {
                cwd: Default::default(),
                mcp_servers: vec![],
                meta: None,
            }))
            .await;

            assert!(
                session_response.is_ok(),
                "Session/new should succeed: {:?}",
                session_response
            );

            let session = session_response.unwrap();
            // ElizACP generates UUID session IDs, just verify it's non-empty
            assert!(!session.session_id.0.is_empty());

            Ok(())
        },
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_agent_handles_prompt() -> Result<(), sacp::Error> {
    // Initialize tracing for debug output
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    // Create channel to collect log events
    let (mut log_tx, mut log_rx) = mpsc::unbounded();

    sacp::ClientToAgent::builder()
        .name("editor-to-connector")
        .on_receive_notification(
            {
                let mut log_tx = log_tx.clone();
                async move |notification: SessionNotification,
                            _cx: sacp::JrConnectionCx<sacp::ClientToAgent>| {
                    // Log the notification in debug format
                    log_tx
                        .send(format!("{notification:?}"))
                        .await
                        .map_err(|_| sacp::Error::internal_error())
                }
            },
            sacp::on_notification!(),
        )
        .connect_to(Conductor::new(
            "mcp-integration-conductor".to_string(),
            vec![
                mcp_integration::proxy::create(),
                sacp::DynComponent::new(ElizaAgent::new()),
            ],
            Default::default(),
        ))?
        .with_client(async |editor_cx| {
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

            tracing::debug!(session_id = %session.session_id.0, "Session created");

            // Send a prompt to call the echo tool via ElizACP's command syntax
            let prompt_response = recv(editor_cx.send_request(PromptRequest {
                session_id: session.session_id.clone(),
                prompt: vec![ContentBlock::Text(TextContent {
                    annotations: None,
                    text: r#"Use tool test::echo with {"message": "Hello from the test!"}"#
                        .to_string(),
                    meta: None,
                })],
                meta: None,
            }))
            .await?;

            // Log the response
            log_tx
                .send(format!("{prompt_response:?}"))
                .await
                .map_err(|_| sacp::Error::internal_error())?;

            Ok(())
        })
        .await?;

    // Drop the sender and collect all log entries
    drop(log_tx);
    let mut log_entries = Vec::new();
    while let Some(entry) = log_rx.next().await {
        log_entries.push(entry);
    }

    // Verify we got a successful tool call response
    // The session ID is a UUID generated by ElizACP, so we check for the tool result pattern
    assert_eq!(log_entries.len(), 2, "Expected notification + response");
    assert!(
        log_entries[0].contains("OK: CallToolResult"),
        "Expected successful tool call, got: {}",
        log_entries[0]
    );
    assert!(
        log_entries[0].contains("Echo: Hello from the test!"),
        "Expected echo result, got: {}",
        log_entries[0]
    );
    assert!(
        log_entries[1].contains("PromptResponse"),
        "Expected prompt response, got: {}",
        log_entries[1]
    );

    Ok(())
}
