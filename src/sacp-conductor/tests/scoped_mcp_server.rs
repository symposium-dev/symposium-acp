//! Test that MCP servers can reference stack-local data.
//!
//! This test demonstrates the new scoped lifetime feature where an MCP tool
//! can capture references to stack-local data (like a Vec) and push to it
//! when the tool is invoked.

use elizacp::ElizaAgent;
use sacp::mcp_server::McpServer;
use sacp::{Agent, Conductor, Proxy, Role, RunWithConnectionTo, Serve};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// Test that an MCP tool can push to a stack-local vector.
///
/// This validates the scoped lifetime feature - the tool closure captures
/// a reference to `collected_values` which lives on the stack.
#[tokio::test]
async fn test_scoped_mcp_server_through_proxy() -> Result<(), sacp::Error> {
    let conductor = ConductorImpl::new_agent(
        "conductor".to_string(),
        ProxiesAndAgent::new(ElizaAgent::new(true)).proxy(ScopedProxy),
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
    // Run the client
    sacp::Client::builder()
        .run_until(
            ConductorImpl::new_agent(
                "conductor".to_string(),
                ProxiesAndAgent::new(ElizaAgent::new(true)),
                McpBridgeMode::default(),
            ),
            async |cx| {
                // Initialize first
                cx.send_request(sacp::schema::InitializeRequest::new(
                    sacp::schema::ProtocolVersion::LATEST,
                ))
                .block_task()
                .await?;

                let collected_values = Mutex::new(Vec::new());
                let result = cx
                    .build_session(".")
                    .with_mcp_server(make_mcp_server::<Agent>(&collected_values))?
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
            },
        )
        .await?;

    Ok(())
}

struct ScopedProxy;

fn make_mcp_server<'a, Counterpart: Role>(
    values: &'a Mutex<Vec<String>>,
) -> McpServer<Counterpart, impl RunWithConnectionTo<Counterpart>> {
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

impl Serve<Conductor> for ScopedProxy {
    async fn serve(self, client: impl Serve<Proxy>) -> Result<(), sacp::Error> {
        // Stack-local data that the MCP tool will push to
        let values: Mutex<Vec<String>> = Mutex::new(Vec::new());

        // Build the MCP server that captures a reference to collected_values
        let mcp_server = make_mcp_server::<sacp::Conductor>(&values);

        Proxy::builder()
            .name("scoped-mcp-server")
            .with_mcp_server(mcp_server)
            .serve(client)
            .await
    }
}
