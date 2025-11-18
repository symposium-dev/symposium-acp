//! Test client helper for conductor integration tests.
//!
//! Provides a simple helper function to connect to an agent, send a prompt,
//! and collect all session updates into a string.

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
    yopo::prompt(
        sacp::ByteStreams::new(outgoing, incoming),
        prompt.to_string(),
    )
    .await
    .map_err(|e| sacp::util::internal_error(e.to_string()))
}
