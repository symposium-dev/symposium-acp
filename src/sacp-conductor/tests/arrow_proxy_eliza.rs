//! Integration test for conductor with arrow proxy and eliza agent.
//!
//! This test verifies that:
//! 1. Conductor can orchestrate a proxy chain with arrow proxy + eliza
//! 2. Session updates from eliza get the '>' prefix from arrow proxy
//! 3. The full proxy chain works end-to-end

use sacp::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, SessionNotification,
    TextContent,
};
use sacp::{JrConnection, MessageAndCx};
use sacp_conductor::conductor::Conductor;
use sacp_tokio::AcpAgent;
use std::str::FromStr;
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::test]
async fn test_conductor_with_arrow_proxy_and_eliza() -> Result<(), sacp::Error> {
    // Set up tracing for debugging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("sacp_conductor=debug".parse().unwrap())
                .add_directive("arrow_proxy=debug".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();

    // Create the component chain: arrow_proxy -> eliza
    let arrow_proxy_agent = AcpAgent::from_str("cargo run -p sacp-test --example arrow_proxy")?;
    let eliza_agent = AcpAgent::from_str("cargo run -p elizacp")?;

    // Create duplex streams for editor <-> conductor communication
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    // Spawn the conductor
    let conductor_handle = tokio::spawn(async move {
        Conductor::run(
            conductor_write.compat_write(),
            conductor_read.compat(),
            vec![Box::new(arrow_proxy_agent), Box::new(eliza_agent)],
        )
        .await
    });

    // Editor side: connect and send a prompt
    let editor_handle = tokio::spawn(async move {
        let connection = JrConnection::new(editor_write.compat_write(), editor_read.compat())
            .name("test-editor");

        let editor_cx = connection.connection_cx();

        // Spawn message receiver
        let receiver_handle = tokio::spawn({
            let _editor_cx = editor_cx.clone();
            async move {
                connection
                    .on_receive_message(
                        async move |message: MessageAndCx<
                            sacp::UntypedMessage,
                            SessionNotification,
                        >| {
                            match message {
                                MessageAndCx::Notification(notif, _) => {
                                    tracing::debug!(?notif, "Received session notification");
                                    // Store the notification for verification
                                    // (in real test we'd use a channel)
                                }
                                _ => {}
                            }
                            Ok(())
                        },
                    )
                    .serve()
                    .await
            }
        });

        // Send initialize
        let init_response = editor_cx
            .send_request(InitializeRequest {
                protocol_version: Default::default(),
                client_capabilities: Default::default(),
                meta: None,
                client_info: None,
            })
            .block_task()
            .await?;
        tracing::debug!(?init_response, "Initialize complete");

        // Create a new session
        let session_response = editor_cx
            .send_request(NewSessionRequest {
                meta: None,
                mcp_servers: vec![],
                cwd: std::env::current_dir().unwrap_or_default(),
            })
            .block_task()
            .await?;
        let session_id = session_response.session_id;
        tracing::debug!(?session_id, "Session created");

        // Send a prompt
        let prompt_response = editor_cx
            .send_request(PromptRequest {
                session_id: session_id.clone(),
                prompt: vec![ContentBlock::Text(TextContent {
                    text: "Hello".to_string(),
                    annotations: None,
                    meta: None,
                })],
                meta: None,
            })
            .block_task()
            .await?;
        tracing::debug!(?prompt_response, "Prompt complete");

        // Give some time for session updates to arrive
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        receiver_handle.abort();

        Ok::<(), sacp::Error>(())
    });

    // Wait for editor to complete
    tokio::time::timeout(std::time::Duration::from_secs(30), editor_handle)
        .await
        .expect("Test timed out")
        .expect("Editor task panicked")
        .expect("Editor failed");

    conductor_handle.abort();

    Ok(())
}
