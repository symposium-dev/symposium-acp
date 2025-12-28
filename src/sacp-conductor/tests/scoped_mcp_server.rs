//! Test that MCP servers can reference stack-local data.
//!
//! This test demonstrates the new scoped lifetime feature where an MCP tool
//! can capture references to stack-local data (like a Vec) and push to it
//! when the tool is invoked.

use elizacp::ElizaAgent;
use sacp::mcp_server::McpServer;
use sacp::{AgentRole, ClientToAgent, Component, HasPeer, JrLink, JrResponder, ProxyToConductor};
use sacp_conductor::{Conductor, McpBridgeMode, ProxiesAndAgent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// Test that an MCP tool can push to a stack-local vector.
///
/// This validates the scoped lifetime feature - the tool closure captures
/// a reference to `collected_values` which lives on the stack.
#[tokio::test]
async fn test_scoped_mcp_server_through_proxy() -> Result<(), sacp::Error> {
    let conductor = Conductor::new_agent(
        "conductor".to_string(),
        ProxiesAndAgent::new(ElizaAgent::new()).proxy(ScopedProxy),
        Default::default(),
    );

    let result = yopo::prompt(
        conductor,
        r#"Use tool test::push with {"elements": ["Hello", "world"]}"#,
    )
    .await?;

    expect_test::expect![[r#"
        "OK: CallToolResult { content: [Annotated { raw: Text(RawTextContent { text: \"2\", meta: None }), annotations: None }], structured_content: None, is_error: Some(false), meta: None }"
    "#]].assert_debug_eq(&result);

    Ok(())
}

/// Test that an MCP tool can push to a stack-local vector through a session.
///
/// This validates the scoped lifetime feature with session-scoped MCP servers.
/// The MCP server captures a reference to stack-local data that lives for
/// the duration of the session.
#[tokio::test]
async fn test_scoped_mcp_server_through_session() -> Result<(), sacp::Error> {
    ClientToAgent::builder()
        .connect_to(Conductor::new_agent("conductor".to_string(), ProxiesAndAgent::new(ElizaAgent::new()), McpBridgeMode::default()))?
        .run_until(async |cx| {
            // Initialize first
            cx.send_request(sacp::schema::InitializeRequest {
                protocol_version: Default::default(),
                client_capabilities: Default::default(),
                client_info: None,
                meta: None,
            })
            .block_task()
            .await?;

            let collected_values = Mutex::new(Vec::new());
            let result = cx
                .build_session(".")
                .with_mcp_server(make_mcp_server(&collected_values))?
                .block_task()
                .run_until(async |mut active_session| {
                    active_session
                        .send_prompt(r#"Use tool test::push with {"elements": ["Hello", "world"]}"#)?;
                    active_session.read_to_string().await
                })
                .await?;

            expect_test::expect![[r#"
                "OK: CallToolResult { content: [Annotated { raw: Text(RawTextContent { text: \"2\", meta: None }), annotations: None }], structured_content: None, is_error: Some(false), meta: None }"
            "#]].assert_debug_eq(&result);

            Ok(())
        }).await?;

    Ok(())
}

struct ScopedProxy;

fn make_mcp_server<'a, Link: JrLink>(
    values: &'a Mutex<Vec<String>>,
) -> McpServer<Link, impl JrResponder<Link>>
where
    Link: HasPeer<AgentRole>,
{
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct PushInput {
        elements: Vec<String>,
    }

    McpServer::builder("test".to_string())
        .instructions("A test MCP server with scoped tool")
        .tool_fn_mut(
            "push",
            "Push a value to the collected values",
            async |input: PushInput, _cx| {
                let mut values = values.lock().expect("not poisoned");
                values.extend(input.elements);
                Ok(values.len())
            },
            sacp::tool_fn_mut!(),
        )
        .tool_fn_mut(
            "get",
            "Get the collected values",
            async |(): (), _cx| {
                let values = values.lock().expect("not poisoned");
                Ok(values.clone())
            },
            sacp::tool_fn_mut!(),
        )
        .build()
}

impl Component<ProxyToConductor> for ScopedProxy {
    async fn serve(
        self,
        client: impl Component<sacp::role::ConductorToProxy>,
    ) -> Result<(), sacp::Error> {
        // Stack-local data that the MCP tool will push to
        let values: Mutex<Vec<String>> = Mutex::new(Vec::new());

        // Build the MCP server that captures a reference to collected_values
        let mcp_server = make_mcp_server(&values);

        ProxyToConductor::builder()
            .name("scoped-mcp-server")
            .with_mcp_server(mcp_server)
            .connect_to(client)?
            .serve()
            .await
    }
}
