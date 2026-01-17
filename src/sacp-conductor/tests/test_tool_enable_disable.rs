//! Integration tests for tool enable/disable functionality
//!
//! These tests verify that `disable_tool`, `enable_tool`, `disable_all_tools`,
//! and `enable_all_tools` correctly filter which tools are visible and callable.

use sacp::Component;
use sacp::ProxyToConductor;
use sacp::link::AgentToClient;
use sacp::mcp_server::McpServer;
use sacp_conductor::{Conductor, ProxiesAndAgent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Input for the echo tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoInput {
    message: String,
}

/// Input for the greet tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GreetInput {
    name: String,
}

/// Empty input for simple tools
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EmptyInput {}

/// Create a proxy with multiple tools, some disabled via deny-list
fn create_proxy_with_disabled_tool() -> Result<sacp::DynComponent<ProxyToConductor>, sacp::Error> {
    let mcp_server = McpServer::builder("test_server".to_string())
        .instructions("Test MCP server with some disabled tools")
        .tool_fn(
            "echo",
            "Echo a message back",
            async |input: EchoInput, _context| Ok(format!("Echo: {}", input.message)),
            sacp::tool_fn!(),
        )
        .tool_fn(
            "greet",
            "Greet someone by name",
            async |input: GreetInput, _context| Ok(format!("Hello, {}!", input.name)),
            sacp::tool_fn!(),
        )
        .tool_fn(
            "secret",
            "A secret tool that should be disabled",
            async |_input: EmptyInput, _context| Ok("This is secret!".to_string()),
            sacp::tool_fn!(),
        )
        .disable_tool("secret")?
        .build();

    Ok(sacp::DynComponent::new(TestProxy { mcp_server }))
}

/// Create a proxy where all tools are disabled except specific ones (allow-list)
fn create_proxy_with_allowlist() -> Result<sacp::DynComponent<ProxyToConductor>, sacp::Error> {
    let mcp_server = McpServer::builder("allowlist_server".to_string())
        .instructions("Test MCP server with allow-list")
        .tool_fn(
            "echo",
            "Echo a message back",
            async |input: EchoInput, _context| Ok(format!("Echo: {}", input.message)),
            sacp::tool_fn!(),
        )
        .tool_fn(
            "greet",
            "Greet someone by name",
            async |input: GreetInput, _context| Ok(format!("Hello, {}!", input.name)),
            sacp::tool_fn!(),
        )
        .tool_fn(
            "secret",
            "A secret tool",
            async |_input: EmptyInput, _context| Ok("This is secret!".to_string()),
            sacp::tool_fn!(),
        )
        .disable_all_tools()
        .enable_tool("echo")?
        .build();

    Ok(sacp::DynComponent::new(TestProxy { mcp_server }))
}

struct TestProxy<R: sacp::Run<ProxyToConductor>> {
    mcp_server: McpServer<ProxyToConductor, R>,
}

impl<R: sacp::Run<ProxyToConductor> + 'static + Send> Component<ProxyToConductor>
    for TestProxy<R>
{
    async fn serve(
        self,
        client: impl Component<sacp::link::ConductorToProxy>,
    ) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("test-proxy")
            .with_mcp_server(self.mcp_server)
            .serve(client)
            .await
    }
}

/// Elizacp agent component wrapper for testing
struct ElizacpAgentComponent;

impl Component<AgentToClient> for ElizacpAgentComponent {
    async fn serve(
        self,
        client: impl Component<sacp::link::ClientToAgent>,
    ) -> Result<(), sacp::Error> {
        let (elizacp_write, client_read) = duplex(8192);
        let (client_write, elizacp_read) = duplex(8192);

        let elizacp_transport =
            sacp::ByteStreams::new(elizacp_write.compat_write(), elizacp_read.compat());

        let client_transport =
            sacp::ByteStreams::new(client_write.compat_write(), client_read.compat());

        tokio::spawn(async move {
            if let Err(e) =
                Component::<AgentToClient>::serve(elizacp::ElizaAgent::new(true), elizacp_transport)
                    .await
            {
                tracing::error!("Elizacp error: {}", e);
            }
        });

        Component::<AgentToClient>::serve(client_transport, client).await
    }
}

// ============================================================================
// Tests for deny-list (disable specific tools)
// ============================================================================

#[tokio::test]
async fn test_list_tools_excludes_disabled() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(ElizacpAgentComponent).proxy(create_proxy_with_disabled_tool()?),
            Default::default(),
        ),
        "List tools from test_server",
    )
    .await?;

    // Should contain echo and greet, but NOT secret
    assert!(result.contains("echo"), "Expected 'echo' tool in list");
    assert!(result.contains("greet"), "Expected 'greet' tool in list");
    assert!(
        !result.contains("secret"),
        "Disabled 'secret' tool should not appear in list"
    );

    Ok(())
}

#[tokio::test]
async fn test_enabled_tool_can_be_called() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(ElizacpAgentComponent).proxy(create_proxy_with_disabled_tool()?),
            Default::default(),
        ),
        r#"Use tool test_server::echo with {"message": "hello"}"#,
    )
    .await?;

    assert!(
        result.contains("Echo: hello"),
        "Expected echo response, got: {}",
        result
    );

    Ok(())
}

#[tokio::test]
async fn test_disabled_tool_returns_not_found() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(ElizacpAgentComponent).proxy(create_proxy_with_disabled_tool()?),
            Default::default(),
        ),
        r#"Use tool test_server::secret with {}"#,
    )
    .await?;

    // Should get an error about tool not found
    assert!(
        result.contains("not found") || result.contains("error"),
        "Expected error for disabled tool, got: {}",
        result
    );

    Ok(())
}

// ============================================================================
// Tests for allow-list (disable all, enable specific)
// ============================================================================

#[tokio::test]
async fn test_allowlist_only_shows_enabled_tools() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(ElizacpAgentComponent).proxy(create_proxy_with_allowlist()?),
            Default::default(),
        ),
        "List tools from allowlist_server",
    )
    .await?;

    // Should only contain echo
    assert!(result.contains("echo"), "Expected 'echo' tool in list");
    assert!(
        !result.contains("greet"),
        "'greet' should not appear (not in allow-list)"
    );
    assert!(
        !result.contains("secret"),
        "'secret' should not appear (not in allow-list)"
    );

    Ok(())
}

#[tokio::test]
async fn test_allowlist_enabled_tool_works() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(ElizacpAgentComponent).proxy(create_proxy_with_allowlist()?),
            Default::default(),
        ),
        r#"Use tool allowlist_server::echo with {"message": "allowed"}"#,
    )
    .await?;

    assert!(
        result.contains("Echo: allowed"),
        "Expected echo response, got: {}",
        result
    );

    Ok(())
}

#[tokio::test]
async fn test_allowlist_non_enabled_tool_returns_not_found() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(ElizacpAgentComponent).proxy(create_proxy_with_allowlist()?),
            Default::default(),
        ),
        r#"Use tool allowlist_server::greet with {"name": "World"}"#,
    )
    .await?;

    // greet is registered but not enabled, should error
    assert!(
        result.contains("not found") || result.contains("error"),
        "Expected error for non-enabled tool, got: {}",
        result
    );

    Ok(())
}
