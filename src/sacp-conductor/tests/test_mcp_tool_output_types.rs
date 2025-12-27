//! Test MCP tools with various output types (string, integer, object)
//!
//! MCP structured output requires JSON objects. This test verifies behavior
//! when tools return non-object types like bare strings or integers.

use sacp::Component;
use sacp::ProxyToConductor;
use sacp::mcp_server::McpServer;
use sacp_conductor::Conductor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Empty input for test tools
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EmptyInput {}

/// Create a proxy with tools that return different types
fn create_test_proxy() -> Result<sacp::DynComponent, sacp::Error> {
    let mcp_server = McpServer::builder("test_server".to_string())
        .instructions("Test MCP server with various output types")
        .tool_fn_mut(
            "return_string",
            "Returns a bare string",
            async |_input: EmptyInput, _context| Ok("hello world".to_string()),
            sacp::tool_fn_mut!(),
        )
        .tool_fn_mut(
            "return_integer",
            "Returns a bare integer",
            async |_input: EmptyInput, _context| Ok(42i32),
            sacp::tool_fn_mut!(),
        )
        .build();

    Ok(sacp::DynComponent::new(ProxyWithTestServer { mcp_server }))
}

struct ProxyWithTestServer<R: sacp::JrResponder<ProxyToConductor>> {
    mcp_server: McpServer<ProxyToConductor, R>,
}

impl<R: sacp::JrResponder<ProxyToConductor> + 'static + Send> Component for ProxyWithTestServer<R> {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("test-proxy")
            .with_mcp_server(self.mcp_server)
            .serve(client)
            .await
    }
}

/// Elizacp agent component wrapper for testing
struct ElizacpAgentComponent;

impl Component for ElizacpAgentComponent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        let (elizacp_write, client_read) = duplex(8192);
        let (client_write, elizacp_read) = duplex(8192);

        let elizacp_transport =
            sacp::ByteStreams::new(elizacp_write.compat_write(), elizacp_read.compat());

        let client_transport =
            sacp::ByteStreams::new(client_write.compat_write(), client_read.compat());

        tokio::spawn(async move {
            if let Err(e) = elizacp::ElizaAgent::new().serve(elizacp_transport).await {
                tracing::error!("Elizacp error: {}", e);
            }
        });

        client_transport.serve(client).await
    }
}

#[tokio::test]
async fn test_tool_returning_string() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            vec![
                create_test_proxy()?,
                sacp::DynComponent::new(ElizacpAgentComponent),
            ],
            Default::default(),
        ),
        r#"Use tool test_server::return_string with {}"#,
    )
    .await?;

    // The result should contain "hello world" somewhere
    assert!(
        result.contains("hello world"),
        "expected 'hello world' in result: {result}"
    );

    Ok(())
}

#[tokio::test]
async fn test_tool_returning_integer() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            vec![
                create_test_proxy()?,
                sacp::DynComponent::new(ElizacpAgentComponent),
            ],
            Default::default(),
        ),
        r#"Use tool test_server::return_integer with {}"#,
    )
    .await?;

    // The result should contain "42" somewhere
    assert!(result.contains("42"), "expected '42' in result: {result}");

    Ok(())
}
