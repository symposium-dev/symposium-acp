//! Integration test verifying that MCP tools receive the correct session_id
//!
//! This test verifies the complete flow:
//! 1. Editor creates a session and receives a session_id
//! 2. Proxy provides an MCP server with an echo tool
//! 3. Elizacp agent invokes the tool
//! 4. The tool receives the correct session_id in its context
//! 5. The tool returns the session_id in its response
//! 6. We verify the session_ids match

use sacp::schema::{ContentBlock, SessionId, TextContent};
use sacp::{Component, JrHandlerChain};
use sacp_conductor::conductor::Conductor;
use sacp_proxy::{AcpProxyExt, McpServer, McpServiceRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Input for the echo tool (null/empty)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoInput {}

/// Output from the echo tool containing the session_id
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoOutput {
    session_id: String,
}

fn conductor_command() -> Vec<String> {
    vec![
        "cargo".to_string(),
        "run".to_string(),
        "-p".to_string(),
        "sacp-conductor".to_string(),
        "--".to_string(),
    ]
}

/// Create a proxy that provides an MCP server with a session_id echo tool
fn create_echo_proxy() -> Result<sacp::DynComponent, sacp::Error> {
    // Create MCP server with an echo tool that returns the session_id
    let mcp_server = McpServer::new()
        .instructions("Test MCP server with session_id echo tool")
        .tool_fn(
            "echo",
            "Returns the current session_id",
            async |_input: EchoInput, context| {
                Ok(EchoOutput {
                    session_id: context.session_id().0.to_string(),
                })
            },
            |f, args, cx| Box::pin(f(args, cx)),
        );

    // Create registry with the server
    let registry = McpServiceRegistry::default().with_mcp_server("echo_server", mcp_server)?;

    // Create proxy component
    Ok(sacp::DynComponent::new(ProxyWithEchoServer { registry }))
}

struct ProxyWithEchoServer {
    registry: McpServiceRegistry,
}

impl Component for ProxyWithEchoServer {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        JrHandlerChain::new()
            .name("echo-proxy")
            .provide_mcp(self.registry)
            .proxy()
            .serve(client)
            .await
    }
}

/// Elizacp agent component wrapper for testing
struct ElizacpAgentComponent;

impl Component for ElizacpAgentComponent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Create duplex channels for bidirectional communication
        let (elizacp_write, client_read) = duplex(8192);
        let (client_write, elizacp_read) = duplex(8192);

        let elizacp_transport =
            sacp::ByteStreams::new(elizacp_write.compat_write(), elizacp_read.compat());

        let client_transport =
            sacp::ByteStreams::new(client_write.compat_write(), client_read.compat());

        // Spawn elizacp in a background task
        tokio::spawn(async move {
            if let Err(e) = elizacp::ElizaAgent::new().serve(elizacp_transport).await {
                tracing::error!("Elizacp error: {}", e);
            }
        });

        // Serve the client with the transport connected to elizacp
        client_transport.serve(client).await
    }
}

#[tokio::test]
async fn test_list_tools_from_mcp_server() -> Result<(), sacp::Error> {
    use expect_test::expect;

    // Create the component chain: proxy with echo server -> eliza
    let proxy = create_echo_proxy()?;
    let eliza = sacp::DynComponent::new(ElizacpAgentComponent);

    // Use yopo to send the prompt and get the response
    let result = yopo::prompt(
        Conductor::new(
            "test-conductor".to_string(),
            vec![proxy, eliza],
            Default::default(),
        ),
        "List tools from echo_server",
    )
    .await?;

    // Check the response using expect_test
    expect![[r#"
        Available tools:
          - echo: Returns the current session_id"#]]
    .assert_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_session_id_delivered_to_mcp_tools() -> Result<(), sacp::Error> {
    // Set up editor <-> conductor communication
    use futures::{SinkExt, StreamExt, channel::mpsc};
    use sacp::schema::SessionNotification;
    use tokio::io::duplex;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    let (editor, conductor) = duplex(8192);
    let (editor_in, editor_out) = tokio::io::split(editor);
    let (conductor_in, conductor_out) = tokio::io::split(conductor);

    let transport = sacp::ByteStreams::new(editor_out.compat_write(), editor_in.compat());

    // Track the session_id we receive from the conductor
    let received_session_id: std::sync::Arc<tokio::sync::Mutex<Option<SessionId>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let session_id_clone = received_session_id.clone();

    // Collect notifications
    let (notif_tx, mut notif_rx) = mpsc::unbounded();

    JrHandlerChain::new()
        .name("test-editor")
        .on_receive_notification({
            let mut notif_tx = notif_tx.clone();
            async move |notification: SessionNotification, _cx| {
                notif_tx
                    .send(notification)
                    .await
                    .map_err(|_| sacp::Error::internal_error())
            }
        })
        .with_spawned(|_cx| async move {
            Conductor::new(
                "test-conductor".to_string(),
                vec![
                    create_echo_proxy()?,
                    sacp::DynComponent::new(ElizacpAgentComponent),
                ],
                Default::default(),
            )
            .run(sacp::ByteStreams::new(
                conductor_out.compat_write(),
                conductor_in.compat(),
            ))
            .await
        })
        .with_client(transport, async move |editor_cx| {
            // Initialize
            editor_cx
                .send_request(sacp::schema::InitializeRequest {
                    protocol_version: Default::default(),
                    client_capabilities: Default::default(),
                    meta: None,
                    client_info: None,
                })
                .block_task()
                .await?;

            // Create session
            let session = editor_cx
                .send_request(sacp::schema::NewSessionRequest {
                    cwd: Default::default(),
                    mcp_servers: vec![],
                    meta: None,
                })
                .block_task()
                .await?;

            // Store the session_id for later comparison
            *session_id_clone.lock().await = Some(session.session_id.clone());

            tracing::info!(session_id = %session.session_id.0, "Session created");

            // Send a prompt asking elizacp to invoke the echo tool
            editor_cx
                .send_request(sacp::schema::PromptRequest {
                    session_id: session.session_id.clone(),
                    prompt: vec![ContentBlock::Text(TextContent {
                        annotations: None,
                        text: r#"Use tool echo_server::echo with {}"#.to_string(),
                        meta: None,
                    })],
                    meta: None,
                })
                .block_task()
                .await?;

            Ok(())
        })
        .await?;

    // Drop sender and collect all notifications
    drop(notif_tx);
    let mut notifications = Vec::new();
    while let Some(notif) = notif_rx.next().await {
        notifications.push(notif);
    }

    // Extract the session_id from tool response
    let mut tool_session_id: Option<String> = None;
    for notif in &notifications {
        if let sacp::schema::SessionUpdate::AgentMessageChunk(chunk) = &notif.update {
            if let ContentBlock::Text(text_content) = &chunk.content {
                let text = &text_content.text;

                // Look for session_id in the text
                // The format is: OK: CallToolResult { ... structured_content: Some(Object {"session_id": String("...")}) ... }
                if text.contains("session_id") {
                    // Try to extract session_id from the structured_content
                    if let Some(start) = text.find(r#""session_id": String(""#) {
                        let start_idx = start + r#""session_id": String(""#.len();
                        if let Some(end_idx) = text[start_idx..].find('"') {
                            let session_id = &text[start_idx..start_idx + end_idx];
                            tool_session_id = Some(session_id.to_string());
                            break;
                        }
                    }
                }
            }
        }
    }

    // Verify the session_ids match
    let expected_session_id = received_session_id
        .lock()
        .await
        .as_ref()
        .expect("Session ID should be set")
        .0
        .clone();

    let actual_session_id = tool_session_id.expect("Tool should have returned session_id");

    assert_eq!(
        expected_session_id.as_ref(),
        actual_session_id.as_str(),
        "Session ID from tool should match the session ID from session/new"
    );

    tracing::info!(
        expected = %expected_session_id,
        actual = %actual_session_id,
        "Session IDs match! Test passed."
    );

    Ok(())
}
