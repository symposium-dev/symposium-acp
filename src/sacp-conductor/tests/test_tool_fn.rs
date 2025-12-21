//! Integration test for `tool_fn` - stateless concurrent tools
//!
//! This test verifies that `tool_fn` works correctly for stateless tools
//! that don't need mutable state.

use sacp::Component;
use sacp::ProxyToConductor;
use sacp::mcp_server::McpServer;
use sacp_conductor::Conductor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Input for the greet tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GreetInput {
    name: String,
}

/// Create a proxy that provides an MCP server with a stateless greet tool
fn create_greet_proxy() -> Result<sacp::DynComponent, sacp::Error> {
    // Create MCP server with a stateless greet tool using tool_fn
    let mcp_server = McpServer::builder("greet_server".to_string())
        .instructions("Test MCP server with stateless greet tool")
        .tool_fn(
            "greet",
            "Greet someone by name",
            async |input: GreetInput, _context| Ok(format!("Hello, {}!", input.name)),
            sacp::tool_fn!(),
        )
        .build();

    // Create proxy component
    Ok(sacp::DynComponent::new(ProxyWithGreetServer { mcp_server }))
}

struct ProxyWithGreetServer<R: sacp::JrResponder<ProxyToConductor>> {
    mcp_server: McpServer<ProxyToConductor, R>,
}

impl<R: sacp::JrResponder<ProxyToConductor> + 'static + Send> Component
    for ProxyWithGreetServer<R>
{
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("greet-proxy")
            .with_mcp_server(self.mcp_server)
            .serve(client)
            .await
    }
}

/// Elizacp agent component wrapper for testing
struct ElizacpAgentComponent;

impl Component for ElizacpAgentComponent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Create duplex channels for bidirectional communication
        let (elizacp_write, client_read) = duplex(8192);
        let (client_write, elizacp_read) = duplex(8192);

        let elizacp_transport =
            sacp::ByteStreams::new(elizacp_write.compat_write(), elizacp_read.compat());

        let client_transport =
            sacp::ByteStreams::new(client_write.compat_write(), client_read.compat());

        // Spawn elizacp in a background task
        tokio::spawn(async move {
            if let Err(e) = elizacp::ElizaAgent::new().serve(elizacp_transport).await {
                tracing::error!("Elizacp error: {}", e);
            }
        });

        // Serve the client with the transport connected to elizacp
        client_transport.serve(client).await
    }
}

#[tokio::test]
async fn test_tool_fn_greet() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new(
            "test-conductor".to_string(),
            vec![
                create_greet_proxy()?,
                sacp::DynComponent::new(ElizacpAgentComponent),
            ],
            Default::default(),
        ),
        r#"Use tool greet_server::greet with {"name": "World"}"#,
    )
    .await?;

    expect_test::expect![[r#"
        "OK: CallToolResult { content: [Annotated { raw: Text(RawTextContent { text: \"\\\"Hello, World!\\\"\", meta: None }), annotations: None }], structured_content: Some(String(\"Hello, World!\")), is_error: Some(false), meta: None }"
    "#]].assert_debug_eq(&result);

    Ok(())
}
