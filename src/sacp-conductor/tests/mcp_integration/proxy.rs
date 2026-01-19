//! Proxy component that provides MCP tools

use sacp::{Conductor, Proxy, ConnectTo};
use sacp::mcp_server::McpServer;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for the echo tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoParams {
    /// The message to echo back
    message: String,
}

/// Output from the echo tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoOutput {
    /// The echoed message
    result: String,
}

pub struct ProxyComponent;

impl ConnectTo<Conductor> for ProxyComponent {
    async fn connect_to(self, client: impl ConnectTo<Proxy>) -> Result<(), sacp::Error> {
        let test_server = McpServer::builder("test")
            .instructions("A simple test MCP server with an echo tool")
            .tool_fn_mut(
                "echo",
                "Echoes back the input message",
                async |params: EchoParams, _context| {
                    Ok(EchoOutput {
                        result: format!("Echo: {}", params.message),
                    })
                },
                sacp::tool_fn_mut!(),
            )
            .build();

        sacp::Proxy.builder()
            .name("proxy-component")
            .with_mcp_server(test_server)
            .connect_to(client)
            .await
    }
}
