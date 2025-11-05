//! Test client helper for conductor integration tests.
//!
//! Provides a simple helper function to connect to an agent, send a prompt,
//! and collect all session updates into a string.

use sacp::JrConnection;
use sacp::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, SessionNotification,
    SessionUpdate, TextContent,
};
use std::sync::{Arc, Mutex};

/// Connect to an agent, send a prompt, and collect all session update text.
///
/// This helper:
/// - Connects to the agent via the provided streams
/// - Initializes the connection
/// - Creates a new session
/// - Sends the prompt
/// - Collects all text from AgentMessageChunk session updates
/// - Returns the concatenated result
///
/// # Example
///
/// ```ignore
/// let (write, read) = create_streams_to_agent();
/// let result = yolo_prompt(write, read, "Hello").await?;
/// assert!(result.contains("response text"));
/// ```
pub async fn yolo_prompt<OB, IB>(
    outgoing: OB,
    incoming: IB,
    prompt: &str,
) -> Result<String, sacp::Error>
where
    OB: futures::AsyncWrite + Send + 'static,
    IB: futures::AsyncRead + Send + 'static,
{
    let collected_text = Arc::new(Mutex::new(String::new()));

    let transport = sacp::ViaBytes::new(outgoing, incoming);
    let connection = JrConnection::new().name("test-client");

    let cx = connection.connection_cx();

    // Spawn session update collector
    let collector_handle = tokio::spawn({
        let collected_text = collected_text.clone();
        async move {
            connection
                .on_receive_notification(async move |notif: SessionNotification, _cx| {
                    // Collect text from AgentMessageChunk updates
                    if let SessionUpdate::AgentMessageChunk(chunk) = &notif.update {
                        if let ContentBlock::Text(text_content) = &chunk.content {
                            collected_text.lock().unwrap().push_str(&text_content.text);
                        }
                    }
                    Ok(())
                })
                .serve(transport)
                .await
        }
    });

    // Initialize
    cx.send_request(InitializeRequest {
        protocol_version: Default::default(),
        client_capabilities: Default::default(),
        meta: None,
        client_info: None,
    })
    .block_task()
    .await?;

    // Create session
    let session_response = cx
        .send_request(NewSessionRequest {
            meta: None,
            mcp_servers: vec![],
            cwd: std::env::current_dir().unwrap_or_default(),
        })
        .block_task()
        .await?;

    // Send prompt
    cx.send_request(PromptRequest {
        session_id: session_response.session_id,
        prompt: vec![ContentBlock::Text(TextContent {
            text: prompt.to_string(),
            annotations: None,
            meta: None,
        })],
        meta: None,
    })
    .block_task()
    .await?;

    // Give time for session updates to arrive
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Stop the collector
    collector_handle.abort();

    // Return collected text
    let result = collected_text.lock().unwrap().clone();
    Ok(result)
}
