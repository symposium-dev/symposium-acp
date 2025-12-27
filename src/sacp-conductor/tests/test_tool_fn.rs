//! Integration test for `tool_fn` - stateless concurrent tools
//!
//! This test verifies that `tool_fn` works correctly for stateless tools
//! that don't need mutable state.

use sacp::Component;
use sacp::ProxyToConductor;
use sacp::mcp_server::McpServer;
use sacp::role::AgentToClient;
use sacp_conductor::{Conductor, ProxiesAndAgent};
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
fn create_greet_proxy() -> Result<sacp::DynComponent<ProxyToConductor>, sacp::Error> {
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

impl<R: sacp::JrResponder<ProxyToConductor> + 'static + Send> Component<ProxyToConductor>
    for ProxyWithGreetServer<R>
{
    async fn serve(
        self,
        client: impl Component<sacp::role::ConductorToProxy>,
    ) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("greet-proxy")
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
        client: impl Component<sacp::role::ClientToAgent>,
    ) -> Result<(), sacp::Error> {
        // Create duplex channels for bidirectional communication
        let (elizacp_write, client_read) = duplex(8192);
        let (client_write, elizacp_read) = duplex(8192);

        let elizacp_transport =
            sacp::ByteStreams::new(elizacp_write.compat_write(), elizacp_read.compat());

        let client_transport =
            sacp::ByteStreams::new(client_write.compat_write(), client_read.compat());

        // Spawn elizacp in a background task
        tokio::spawn(async move {
            if let Err(e) =
                Component::<AgentToClient>::serve(elizacp::ElizaAgent::new(), elizacp_transport)
                    .await
            {
                tracing::error!("Elizacp error: {}", e);
            }
        });

        // Serve the client with the transport connected to elizacp
        Component::<AgentToClient>::serve(client_transport, client).await
    }
}

#[tokio::test]
async fn test_tool_fn_greet() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(ElizacpAgentComponent).proxy(create_greet_proxy()?),
            Default::default(),
        ),
        r#"Use tool greet_server::greet with {"name": "World"}"#,
    )
    .await?;

    expect_test::expect![[r#"
        "OK: CallToolResult { content: [Annotated { raw: Text(RawTextContent { text: \"\\\"Hello, World!\\\"\", meta: None }), annotations: None }], structured_content: None, is_error: Some(false), meta: None }"
    "#]].assert_debug_eq(&result);

    Ok(())
}
