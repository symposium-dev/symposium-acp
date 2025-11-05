//! YOLO one-shot client: A simple ACP client that runs a single prompt against an agent.
//!
//! This client:
//! - Takes a prompt and agent configuration as arguments
//! - Spawns the agent
//! - Sends the prompt
//! - Auto-approves all permission requests
//! - Prints all session updates to stdout
//! - Runs until the agent completes
//!
//! # Usage
//!
//! With a command:
//! ```bash
//! cargo run --example yolo_one_shot_client -- "What is 2+2?" "python my_agent.py"
//! ```
//!
//! With JSON config:
//! ```bash
//! cargo run --example yolo_one_shot_client -- "Hello!" '{"type":"stdio","name":"my-agent","command":"python","args":["agent.py"],"env":[]}'
//! ```

use sacp::JrConnection;
use sacp::schema::{
    ContentBlock, InitializeRequest, McpServer, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionNotification, TextContent, VERSION as PROTOCOL_VERSION,
};
use std::path::PathBuf;
use tokio::process::Child;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

// NOTE: This code is inlined here to avoid a circular dependency between `sacp` and `sacp-tokio`.
// If you're writing your own client, you can use `sacp_tokio::AcpAgent` and `JrConnectionExt::to_agent()`
// to simplify this setup significantly.

/// Parse agent configuration from either a command string or JSON.
fn parse_agent_config(s: &str) -> Result<McpServer, Box<dyn std::error::Error>> {
    let trimmed = s.trim();

    // If it starts with '{', try to parse as JSON
    if trimmed.starts_with('{') {
        let server: McpServer = serde_json::from_str(trimmed)?;
        return Ok(server);
    }

    // Otherwise, parse as a command string
    let parts = shell_words::split(trimmed)?;
    if parts.is_empty() {
        return Err("Command string cannot be empty".into());
    }

    let command = PathBuf::from(&parts[0]);
    let args = parts[1..].to_vec();
    let name = command
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("agent")
        .to_string();

    Ok(McpServer::Stdio {
        name,
        command,
        args,
        env: vec![],
    })
}

/// Spawn a process for the agent and get stdio streams.
fn spawn_agent_process(
    server: &McpServer,
) -> Result<
    (
        tokio::process::ChildStdin,
        tokio::process::ChildStdout,
        Child,
    ),
    Box<dyn std::error::Error>,
> {
    match server {
        McpServer::Stdio {
            command,
            args,
            env,
            name: _,
        } => {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(args);
            for env_var in env {
                cmd.env(&env_var.name, &env_var.value);
            }
            cmd.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped());

            let mut child = cmd.spawn()?;
            let child_stdin = child.stdin.take().ok_or("Failed to open stdin")?;
            let child_stdout = child.stdout.take().ok_or("Failed to open stdout")?;

            Ok((child_stdin, child_stdout, child))
        }
        McpServer::Http { .. } => Err("HTTP transport not yet supported".into()),
        McpServer::Sse { .. } => Err("SSE transport not yet supported".into()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <prompt> <agent-config>", args[0]);
        eprintln!();
        eprintln!("  <prompt>       - The prompt to send to the agent");
        eprintln!("  <agent-config> - Either a command string or JSON (starting with '{{')");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} \"What is 2+2?\" \"python my_agent.py\"", args[0]);
        eprintln!(
            "  {} \"Hello!\" '{{\"type\":\"stdio\",\"name\":\"agent\",\"command\":\"python\",\"args\":[\"agent.py\"],\"env\":[]}}'",
            args[0]
        );
        std::process::exit(1);
    }

    let prompt = &args[1];
    let agent_config = &args[2];

    // Parse the agent configuration
    let server = parse_agent_config(agent_config)?;

    eprintln!("🚀 Spawning agent and connecting...");

    // Spawn the agent process
    let (child_stdin, child_stdout, mut child) = spawn_agent_process(&server)?;

    // Create a JrConnection with the agent's stdio streams
    // NOTE: Using sacp_tokio::JrConnectionExt::to_agent() would simplify this setup
    let connection = JrConnection::new(child_stdin.compat_write(), child_stdout.compat());

    // Run the client
    connection
        .on_receive_notification(async move |notification: SessionNotification, _cx| {
            // Print session updates to stdout (so 2>/dev/null shows only agent output)
            println!("{:?}", notification.update);
            Ok(())
        })
        .on_receive_request(async move |request: RequestPermissionRequest, request_cx| {
            // YOLO: Auto-approve all permission requests by selecting the first option
            eprintln!("✅ Auto-approving permission request: {:?}", request);
            let option_id = request.options.first().map(|opt| opt.id.clone());
            match option_id {
                Some(id) => request_cx.respond(RequestPermissionResponse {
                    outcome: RequestPermissionOutcome::Selected { option_id: id },
                    meta: None,
                }),
                None => {
                    eprintln!("⚠️ No options provided in permission request, cancelling");
                    request_cx.respond(RequestPermissionResponse {
                        outcome: RequestPermissionOutcome::Cancelled,
                        meta: None,
                    })
                }
            }
        })
        .with_client(|cx: sacp::JrConnectionCx| async move {
            // Initialize the agent
            eprintln!("🤝 Initializing agent...");
            let init_response = cx
                .send_request(InitializeRequest {
                    protocol_version: PROTOCOL_VERSION,
                    client_capabilities: Default::default(),
                    client_info: Default::default(),
                    meta: None,
                })
                .block_task()
                .await?;

            eprintln!("✓ Agent initialized: {:?}", init_response.agent_info);

            // Create a new session
            eprintln!("📝 Creating new session...");
            let new_session_response = cx
                .send_request(NewSessionRequest {
                    mcp_servers: vec![],
                    cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
                    meta: None,
                })
                .block_task()
                .await?;

            let session_id = new_session_response.session_id;
            eprintln!("✓ Session created: {}", session_id);

            // Send the prompt
            eprintln!("💬 Sending prompt: \"{}\"", prompt);
            let prompt_response = cx
                .send_request(PromptRequest {
                    session_id: session_id.clone(),
                    prompt: vec![ContentBlock::Text(TextContent {
                        text: prompt.to_string(),
                        annotations: None,
                        meta: None,
                    })],
                    meta: None,
                })
                .block_task()
                .await?;

            eprintln!("✅ Agent completed!");
            eprintln!("Stop reason: {:?}", prompt_response.stop_reason);

            Ok(())
        })
        .await?;

    // Kill the child process when done
    let _ = child.kill().await;

    Ok(())
}
