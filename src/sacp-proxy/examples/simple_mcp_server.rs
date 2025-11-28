//! Example of creating a simple MCP server using the sacp-proxy builder API
//!
//! This demonstrates how to create an MCP server with custom tools using the
//! convenient `tool_fn` API that gives tools access to the session context.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use sacp_proxy::McpServer;

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

/// Empty input for tools that don't need parameters
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EmptyInput {}

/// Output containing session information
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SessionInfoOutput {
    acp_url: String,
}

fn main() {
    // Build an MCP server with multiple tools using the convenient tool_fn API
    let _server = McpServer::new()
        .instructions("A simple MCP server with echo and session info tools")
        .tool_fn(
            "echo",
            "Echoes back the input message with session information",
            async |input: EchoInput, context| {
                Ok(EchoOutput {
                    result: format!("ACP {}: Echo: {}", context.acp_url(), input.message),
                })
            },
            |f, args, cx| Box::pin(f(args, cx)),
        )
        .tool_fn(
            "get_session_info",
            "Returns information about the current session",
            async |_input: EmptyInput, context| {
                Ok(SessionInfoOutput {
                    acp_url: context.acp_url(),
                })
            },
            |f, args, cx| Box::pin(f(args, cx)),
        );

    println!("MCP server created successfully!");
    println!("Tools available:");
    println!("  - echo: Echoes back messages with session info");
    println!("  - get_session_info: Returns current session ID");
    println!();
    println!("This is a demonstration - use with sacp-proxy to serve over ACP.");
}
