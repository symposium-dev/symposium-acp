//! YOPO (You Only Prompt Once) - A simple library for testing SACP agents
//!
//! Provides a convenient API for running one-shot prompts against SACP components.

use sacp::schema::{
    AudioContent, ContentBlock, EmbeddedResourceResource, ImageContent, InitializeRequest,
    NewSessionRequest, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionNotification, TextContent, VERSION as PROTOCOL_VERSION,
};
use sacp::{Component, JrHandlerChain};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Converts a `ContentBlock` to its string representation.
///
/// This function provides standard string conversions for different content types:
/// - `Text`: Returns the text content
/// - `Image`: Returns a placeholder like `[Image: image/png]`
/// - `Audio`: Returns a placeholder like `[Audio: audio/wav]`
/// - `ResourceLink`: Returns the URI
/// - `Resource`: Returns the URI
///
/// # Example
///
/// ```no_run
/// use yopo::content_block_to_string;
/// use sacp::schema::{ContentBlock, TextContent};
///
/// let block = ContentBlock::Text(TextContent {
///     annotations: None,
///     text: "Hello".to_string(),
///     meta: None,
/// });
/// assert_eq!(content_block_to_string(&block), "Hello");
/// ```
pub fn content_block_to_string(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(TextContent { text, .. }) => text.clone(),
        ContentBlock::Image(ImageContent { mime_type, .. }) => {
            format!("[Image: {}]", mime_type)
        }
        ContentBlock::Audio(AudioContent { mime_type, .. }) => {
            format!("[Audio: {}]", mime_type)
        }
        ContentBlock::ResourceLink(link) => link.uri.clone(),
        ContentBlock::Resource(resource) => match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(text) => text.uri.clone(),
            EmbeddedResourceResource::BlobResourceContents(blob) => blob.uri.clone(),
        },
    }
}

/// Runs a single prompt against a component with a callback for each content block.
///
/// This function:
/// - Spawns the component
/// - Initializes the agent
/// - Creates a new session
/// - Sends the prompt
/// - Auto-approves all permission requests
/// - Calls the callback with each `ContentBlock` from agent messages
/// - Returns when the prompt completes
///
/// The callback receives each `ContentBlock` as it arrives and can process it
/// asynchronously (e.g., print it, accumulate it, etc.).
///
/// # Example
///
/// ```no_run
/// use yopo::{prompt_with_callback, content_block_to_string};
/// use sacp_tokio::AcpAgent;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let agent = AcpAgent::stdio("my-agent", "python", vec!["agent.py"], vec![]);
/// prompt_with_callback(agent, "What is 2+2?".to_string(), |block| async move {
///     print!("{}", content_block_to_string(&block));
/// }).await?;
/// # Ok(())
/// # }
/// ```
pub async fn prompt_with_callback<C, F, Fut>(
    component: C,
    prompt_text: impl ToString,
    callback: F,
) -> Result<(), sacp::Error>
where
    C: Component,
    F: Fn(ContentBlock) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    // Convert prompt to String
    let prompt_text = prompt_text.to_string();

    // Wrap callback in Arc so it can be cloned into the async block
    let callback = Arc::new(callback);

    // Run the client
    JrHandlerChain::new()
        .on_receive_notification(async move |notification: SessionNotification, _cx| {
            // Call the callback for each agent message chunk
            if let sacp::schema::SessionUpdate::AgentMessageChunk(content_chunk) =
                notification.update
            {
                callback(content_chunk.content).await;
            }
            Ok(())
        })
        .on_receive_request(async move |request: RequestPermissionRequest, request_cx| {
            // Auto-approve all permission requests by selecting the first option
            let outcome = if let Some(option) = request.options.first() {
                RequestPermissionOutcome::Selected {
                    option_id: option.id.clone(),
                }
            } else {
                RequestPermissionOutcome::Cancelled
            };

            request_cx.respond(RequestPermissionResponse {
                outcome,
                meta: None,
            })
        })
        .connect_to(component)?
        .with_client(|cx: sacp::JrConnectionCx| async move {
            // Initialize the agent
            let _init_response = cx
                .send_request(InitializeRequest {
                    protocol_version: PROTOCOL_VERSION,
                    client_capabilities: Default::default(),
                    client_info: None,
                    meta: None,
                })
                .block_task()
                .await?;

            // Create a new session
            let new_session_response = cx
                .send_request(NewSessionRequest {
                    cwd: PathBuf::from("."),
                    mcp_servers: vec![],
                    meta: None,
                })
                .block_task()
                .await?;

            let session_id = new_session_response.session_id;

            // Send the prompt
            let _prompt_response = cx
                .send_request(PromptRequest {
                    session_id,
                    prompt: vec![ContentBlock::Text(TextContent {
                        annotations: None,
                        text: prompt_text,
                        meta: None,
                    })],
                    meta: None,
                })
                .block_task()
                .await?;

            Ok(())
        })
        .await?;

    Ok(())
}

/// Runs a single prompt against a component and returns the accumulated text response.
///
/// This function:
/// - Spawns the component
/// - Initializes the agent
/// - Creates a new session
/// - Sends the prompt
/// - Auto-approves all permission requests
/// - Accumulates all content from agent messages using [`content_block_to_string`]
/// - Returns the complete response as a String
///
/// This is a convenience wrapper around [`prompt_with_callback`] that accumulates
/// all content blocks into a single string.
///
/// # Example
///
/// ```no_run
/// use yopo::prompt;
/// use sacp_tokio::AcpAgent;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let agent = AcpAgent::stdio("my-agent", "python", vec!["agent.py"], vec![]);
/// let response = prompt(agent, "What is 2+2?".to_string()).await?;
/// assert!(response.contains("4"));
/// # Ok(())
/// # }
/// ```
pub async fn prompt(
    component: impl Component,
    prompt_text: impl ToString,
) -> Result<String, sacp::Error> {
    let accumulated_text = Arc::new(Mutex::new(String::new()));
    let accumulated_text_clone = accumulated_text.clone();

    prompt_with_callback(component, prompt_text, move |block| {
        let accumulated_text = accumulated_text_clone.clone();
        async move {
            let text = content_block_to_string(&block);
            accumulated_text.lock().unwrap().push_str(&text);
        }
    })
    .await?;

    // Extract the accumulated text
    let result = Arc::try_unwrap(accumulated_text)
        .map(|mutex| mutex.into_inner().unwrap())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone());

    Ok(result)
}
