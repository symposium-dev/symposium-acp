//! Proxy component that provides MCP tools

use sacp::{Component, JrHandlerChain};
use sacp_proxy::{AcpProxyExt, McpServer, McpServiceRegistry};
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

struct ProxyComponent;

impl Component for ProxyComponent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        let test_server = McpServer::new()
            .instructions("A simple test MCP server with an echo tool")
            .tool_fn(
                "echo",
                "Echoes back the input message",
                async |params: EchoParams, _context| {
                    Ok(EchoOutput {
                        result: format!("Echo: {}", params.message),
                    })
                },
                |f, args, cx| Box::pin(f(args, cx)),
            );

        JrHandlerChain::new()
            .name("proxy-component")
            .provide_mcp(McpServiceRegistry::default().with_mcp_server("test", test_server)?)
            .proxy()
            .serve(client)
            .await
    }
}

pub fn create() -> sacp::DynComponent {
    sacp::DynComponent::new(ProxyComponent)
}
