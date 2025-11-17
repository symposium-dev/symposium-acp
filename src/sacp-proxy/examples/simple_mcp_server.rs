//! Example of creating a simple MCP server using the sacp-proxy builder API
//!
//! This demonstrates how to create an MCP server with custom tools that have
//! access to the session context.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use sacp_proxy::{McpContext, McpServer, McpTool};

/// Input parameters for the echo tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoInput {
    /// The message to echo back
    message: String,
}

/// Output from the echo tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoOutput {
    /// The echoed message with session context
    result: String,
}

/// A simple echo tool that includes session information
struct EchoTool;

impl McpTool for EchoTool {
    type Input = EchoInput;
    type Output = EchoOutput;

    fn name(&self) -> String {
        "echo".to_string()
    }

    fn description(&self) -> String {
        "Echoes back the input message with session information".to_string()
    }

    fn title(&self) -> Option<String> {
        Some("Echo Tool".to_string())
    }

    async fn call_tool(
        &self,
        input: Self::Input,
        context: McpContext,
    ) -> Result<Self::Output, sacp::Error> {
        Ok(EchoOutput {
            result: format!(
                "Session {}: Echo: {}",
                context.session_id().0,
                input.message
            ),
        })
    }
}

/// A tool that returns the current session ID
struct SessionInfoTool;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EmptyInput {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SessionInfoOutput {
    session_id: String,
}

impl McpTool for SessionInfoTool {
    type Input = EmptyInput;
    type Output = SessionInfoOutput;

    fn name(&self) -> String {
        "get_session_info".to_string()
    }

    fn description(&self) -> String {
        "Returns information about the current session".to_string()
    }

    async fn call_tool(
        &self,
        _input: Self::Input,
        context: McpContext,
    ) -> Result<Self::Output, sacp::Error> {
        Ok(SessionInfoOutput {
            session_id: context.session_id().0.to_string(),
        })
    }
}

fn main() {
    // Build an MCP server with multiple tools
    let _server = McpServer::new()
        .instructions("A simple MCP server with echo and session info tools")
        .tool(EchoTool)
        .tool(SessionInfoTool);

    println!("MCP server created successfully!");
    println!("Tools available:");
    println!("  - echo: Echoes back messages with session info");
    println!("  - get_session_info: Returns current session ID");
    println!();
    println!("This is a demonstration - use with sacp-proxy to serve over ACP.");
}
