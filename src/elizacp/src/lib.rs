pub mod eliza;

use anyhow::Result;
use eliza::Eliza;
use sacp::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, McpServer, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent,
};
use sacp::{Component, JrHandlerChain};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Session data for each active session
#[derive(Clone)]
struct SessionData {
    eliza: Eliza,
    mcp_servers: Vec<McpServer>,
}

/// Shared state across all sessions
#[derive(Clone)]
pub struct ElizaAgent {
    sessions: Arc<Mutex<HashMap<SessionId, SessionData>>>,
}

impl ElizaAgent {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn create_session(&self, session_id: &SessionId, mcp_servers: Vec<McpServer>) {
        let mcp_server_count = mcp_servers.len();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(
            session_id.clone(),
            SessionData {
                eliza: Eliza::new(),
                mcp_servers,
            },
        );
        tracing::info!(
            "Created session: {} with {} MCP servers",
            session_id,
            mcp_server_count
        );
    }

    fn get_response(&self, session_id: &SessionId, input: &str) -> Option<String> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .get_mut(session_id)
            .map(|session| session.eliza.respond(input))
    }

    fn get_mcp_servers(&self, session_id: &SessionId) -> Option<Vec<McpServer>> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(session_id)
            .map(|session| session.mcp_servers.clone())
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
        self.create_session(&session_id, request.mcp_servers);

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

        // For Eliza, we just create a fresh session with no MCP servers
        self.create_session(&request.session_id, vec![]);

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

        // Check for MCP commands first before invoking Eliza
        let final_response = if let Some(server_name) = parse_list_tools_command(&input_text) {
            // List tools from a specific server
            tracing::debug!("Listing tools from MCP server: {}", server_name);

            match self.list_tools(session_id, &server_name).await {
                Ok(tools) => format!("Available tools:\n{}", tools),
                Err(e) => format!("ERROR: {}", e),
            }
        } else if let Some((server_name, tool_name, params_json)) = parse_tool_call(&input_text) {
            // Execute the tool call
            tracing::debug!(
                "Executing MCP tool call: {}::{} with params: {}",
                server_name,
                tool_name,
                params_json
            );

            match self
                .execute_tool_call(session_id, &server_name, &tool_name, &params_json)
                .await
            {
                Ok(result) => format!("OK: {}", result),
                Err(e) => format!("ERROR: {}", e),
            }
        } else {
            // Not an MCP command, use Eliza for response
            let response_text = self
                .get_response(session_id, &input_text)
                .unwrap_or_else(|| {
                    format!(
                        "Error: Session {} not found. Please start a new session.",
                        session_id
                    )
                });

            tracing::debug!("Eliza response: {}", response_text);
            response_text
        };

        request_cx
            .connection_cx()
            .send_notification(SessionNotification {
                session_id: session_id.clone(),
                update: SessionUpdate::AgentMessageChunk(ContentChunk {
                    content: final_response.into(),
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

    /// Helper function to execute an operation with an MCP client
    async fn with_mcp_client<F, Fut, T>(
        &self,
        session_id: &SessionId,
        server_name: &str,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(rmcp::service::RunningService<rmcp::RoleClient, ()>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        use rmcp::{
            ServiceExt,
            transport::{ConfigureCommandExt, TokioChildProcess},
        };
        use tokio::process::Command;

        // Get MCP servers for this session
        let mcp_servers = self
            .get_mcp_servers(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        // Find the requested server
        let mcp_server = mcp_servers
            .iter()
            .find(|server| match server {
                McpServer::Stdio { name, .. } => name == server_name,
                McpServer::Http { name, .. } => name == server_name,
                McpServer::Sse { name, .. } => name == server_name,
            })
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found", server_name))?;

        // Spawn MCP client based on server type
        match mcp_server {
            McpServer::Stdio {
                command, args, env, ..
            } => {
                tracing::debug!(
                    command = ?command,
                    args = ?args,
                    server_name = %server_name,
                    "Starting MCP client"
                );

                // Create MCP client by spawning the process
                let mcp_client = ()
                    .serve(TokioChildProcess::new(Command::new(command).configure(
                        |cmd| {
                            cmd.args(args);
                            for env_var in env {
                                cmd.env(&env_var.name, &env_var.value);
                            }
                        },
                    ))?)
                    .await?;

                tracing::debug!("MCP client connected");

                // Execute the operation
                let result = operation(mcp_client).await?;

                Ok(result)
            }
            McpServer::Http { .. } => Err(anyhow::anyhow!("HTTP MCP servers not yet supported")),
            McpServer::Sse { .. } => Err(anyhow::anyhow!("SSE MCP servers not yet supported")),
        }
    }

    async fn list_tools(&self, session_id: &SessionId, server_name: &str) -> Result<String> {
        self.with_mcp_client(session_id, server_name, async move |mcp_client| {
            // List the tools
            let tools_result = mcp_client.list_tools(None).await?;

            tracing::debug!("Tools result: {:?}", tools_result);

            // Clean up the client
            mcp_client.cancel().await?;

            // Format the tools list
            let tools_list = tools_result
                .tools
                .iter()
                .map(|tool| {
                    format!(
                        "  - {}: {}",
                        tool.name,
                        tool.description.as_deref().unwrap_or("No description")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");

            Ok(tools_list)
        })
        .await
    }

    async fn execute_tool_call(
        &self,
        session_id: &SessionId,
        server_name: &str,
        tool_name: &str,
        params_json: &str,
    ) -> Result<String> {
        use rmcp::model::CallToolRequestParam;

        // Parse params JSON
        let params = serde_json::from_str::<serde_json::Value>(params_json)
            .map_err(|e| anyhow::anyhow!("Invalid JSON params: {}", e))?;

        let params_obj = params.as_object().cloned();
        let tool_name = tool_name.to_string();

        self.with_mcp_client(session_id, server_name, async move |mcp_client| {
            tracing::debug!("Calling tool: {}", tool_name);

            // Call the tool
            let tool_result = mcp_client
                .call_tool(CallToolRequestParam {
                    name: tool_name.into(),
                    arguments: params_obj,
                })
                .await?;

            tracing::debug!("Tool call result: {:?}", tool_result);

            // Clean up the client
            mcp_client.cancel().await?;

            // Format the result
            Ok(format!("{:?}", tool_result))
        })
        .await
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

/// Parse list tools command from text input
/// Format: "List tools from <server>"
/// Returns: Some(server_name)
fn parse_list_tools_command(input: &str) -> Option<String> {
    use regex::Regex;

    let re = Regex::new(r"(?i)^list tools from ([a-zA-Z_0-9]+)$").ok()?;
    let captures = re.captures(input.trim())?;

    Some(captures.get(1)?.as_str().to_string())
}

/// Parse tool call command from text input
/// Format: "Use tool <server>::<tool> with <json_params>"
/// Returns: Some((server_name, tool_name, params_json))
fn parse_tool_call(input: &str) -> Option<(String, String, String)> {
    use regex::Regex;

    let re = Regex::new(r"(?i)^use tool ([a-zA-Z_0-9]+)::([a-zA-Z_0-9]+) with (.+)$").ok()?;
    let captures = re.captures(input.trim())?;

    Some((
        captures.get(1)?.as_str().to_string(),
        captures.get(2)?.as_str().to_string(),
        captures.get(3)?.as_str().to_string(),
    ))
}

impl Component for ElizaAgent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        JrHandlerChain::new()
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
            .on_receive_request({
                let agent = self.clone();
                async move |request: NewSessionRequest, request_cx| {
                    agent.handle_new_session(request, request_cx).await
                }
            })
            .on_receive_request({
                let agent = self.clone();
                async move |request: LoadSessionRequest, request_cx| {
                    agent.handle_load_session(request, request_cx).await
                }
            })
            .on_receive_request({
                let agent = self.clone();
                async move |request: PromptRequest, request_cx| {
                    agent.handle_prompt_request(request, request_cx).await
                }
            })
            .connect_to(client)?
            .serve()
            .await
    }
}
