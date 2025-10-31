mod eliza;

use anyhow::Result;
use clap::Parser;
use eliza::Eliza;
use sacp::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    JrConnection, LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{stdin, stdout};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about = "Eliza chatbot as an ACP agent", long_about = None)]
struct Args {
    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing to stderr
    let env_filter = if args.debug {
        EnvFilter::new("elizacp=debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("elizacp=info"))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(std::io::stderr),
        )
        .init();

    tracing::info!("Elizacp starting");

    let agent = ElizaAgent::new();

    // Set up JSON-RPC connection over stdio
    JrConnection::new(stdout().compat_write(), stdin().compat())
        .name("elizacp")
        .on_receive_request({
            async |initialize: InitializeRequest, request_cx| {
                tracing::debug!("Received initialize request");

                request_cx.respond(InitializeResponse {
                    protocol_version: initialize.protocol_version,
                    agent_capabilities: AgentCapabilities {
                        load_session: Default::default(),
                        prompt_capabilities: Default::default(),
                        mcp_capabilities: Default::default(),
                        meta: Default::default(),
                    },
                    auth_methods: Default::default(),
                    agent_info: Default::default(),
                    meta: Default::default(),
                })
            }
        })
        .on_receive_request(async |request: NewSessionRequest, request_cx| {
            agent.handle_new_session(request, request_cx).await
        })
        .on_receive_request(async |request: LoadSessionRequest, request_cx| {
            agent.handle_load_session(request, request_cx).await
        })
        .on_receive_request(async |request: PromptRequest, request_cx| {
            agent.handle_prompt_request(request, request_cx).await
        })
        .serve()
        .await?;

    Ok(())
}

/// Shared state across all sessions
#[derive(Clone)]
struct ElizaAgent {
    sessions: Arc<Mutex<HashMap<SessionId, Eliza>>>,
}

impl ElizaAgent {
    fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn create_session(&self, session_id: &SessionId) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(session_id.clone(), Eliza::new());
        tracing::info!("Created session: {}", session_id);
    }

    fn get_response(&self, session_id: &SessionId, input: &str) -> Option<String> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .get_mut(session_id)
            .map(|eliza| eliza.respond(input))
    }

    fn _end_session(&self, session_id: &SessionId) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.remove(session_id);
        tracing::info!("Ended session: {}", session_id);
    }

    async fn handle_new_session(
        &self,
        request: NewSessionRequest,
        request_cx: sacp::JrRequestCx<NewSessionResponse>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!("New session request with cwd: {:?}", request.cwd);

        // Generate a new session ID
        let session_id = SessionId(Arc::from(uuid::Uuid::new_v4().to_string()));
        self.create_session(&session_id);

        let response = NewSessionResponse {
            session_id,
            modes: None,
            meta: None,
        };

        request_cx.respond(response)
    }

    async fn handle_load_session(
        &self,
        request: LoadSessionRequest,
        request_cx: sacp::JrRequestCx<LoadSessionResponse>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!("Load session request: {:?}", request.session_id);

        // For Eliza, we just create a fresh session
        self.create_session(&request.session_id);

        let response = LoadSessionResponse {
            modes: None,
            meta: None,
        };

        request_cx.respond(response)
    }

    async fn handle_prompt_request(
        &self,
        request: PromptRequest,
        request_cx: sacp::JrRequestCx<PromptResponse>,
    ) -> Result<(), sacp::Error> {
        let session_id = &request.session_id;

        tracing::debug!(
            "Received prompt in session {}: {} content blocks",
            session_id,
            request.prompt.len()
        );

        // Extract text from the prompt
        let input_text = extract_text_from_prompt(&request.prompt);

        // Get Eliza's response
        let response_text = self
            .get_response(session_id, &input_text)
            .unwrap_or_else(|| {
                format!(
                    "Error: Session {} not found. Please start a new session.",
                    session_id
                )
            });

        tracing::debug!("Eliza response: {}", response_text);

        request_cx.send_notification(SessionNotification {
            session_id: session_id.clone(),
            update: SessionUpdate::AgentMessageChunk(ContentChunk {
                content: response_text.into(),
                meta: None,
            }),
            meta: None,
        })?;

        // Complete the request
        request_cx.respond(PromptResponse {
            stop_reason: StopReason::EndTurn,
            meta: None,
        })
    }
}

/// Extract text content from prompt blocks
fn extract_text_from_prompt(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(TextContent { text, .. }) => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}
