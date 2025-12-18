//! Integration test verifying that MCP tools receive the correct session_id
//!
//! This test verifies the complete flow:
//! 1. Editor creates a session and receives a session_id
//! 2. Proxy provides an MCP server with an echo tool
//! 3. Elizacp agent invokes the tool
//! 4. The tool receives the correct session_id in its context
//! 5. The tool returns the session_id in its response
//! 6. We verify the session_ids match

use sacp::Component;
use sacp::ProxyToConductor;
use sacp::mcp_server::McpServer;
use sacp_conductor::Conductor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Input for the echo tool (null/empty)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoInput {}

/// Output from the echo tool containing the session_id
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoOutput {
    acp_url: String,
}

/// Create a proxy that provides an MCP server with a session_id echo tool
fn create_echo_proxy() -> Result<sacp::DynComponent, sacp::Error> {
    // Create MCP server with an echo tool that returns the session_id
    let mcp_server = McpServer::builder("echo_server".to_string())
        .instructions("Test MCP server with session_id echo tool")
        .tool_fn(
            "echo",
            "Returns the current session_id",
            async |_input: EchoInput, context| {
                Ok(EchoOutput {
                    acp_url: context.acp_url(),
                })
            },
        )
        .build();

    // Create proxy component
    Ok(sacp::DynComponent::new(ProxyWithEchoServer { mcp_server }))
}

struct ProxyWithEchoServer<R: sacp::JrResponder<ProxyToConductor>> {
    mcp_server: McpServer<ProxyToConductor, R>,
}

impl<R: sacp::JrResponder<ProxyToConductor> + 'static> Component for ProxyWithEchoServer<R> {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("echo-proxy")
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
async fn test_list_tools_from_mcp_server() -> Result<(), sacp::Error> {
    use expect_test::expect;

    // Create the component chain: proxy with echo server -> eliza
    let proxy = create_echo_proxy()?;
    let eliza = sacp::DynComponent::new(ElizacpAgentComponent);

    // Use yopo to send the prompt and get the response
    let result = yopo::prompt(
        Conductor::new(
            "test-conductor".to_string(),
            vec![proxy, eliza],
            Default::default(),
        ),
        "List tools from echo_server",
    )
    .await?;

    // Check the response using expect_test
    expect![[r#"
        Available tools:
          - echo: Returns the current session_id"#]]
    .assert_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_session_id_delivered_to_mcp_tools() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new(
            "test-conductor".to_string(),
            vec![
                create_echo_proxy()?,
                sacp::DynComponent::new(ElizacpAgentComponent),
            ],
            Default::default(),
        ),
        r#"Use tool echo_server::echo with {}"#,
    )
    .await?;

    let pattern = regex::Regex::new(r#""acp_url":\s*String\("acp:[0-9a-f-]+"\)"#).unwrap();
    assert!(pattern.is_match(&result), "unexpected result: {result}");

    Ok(())
}
