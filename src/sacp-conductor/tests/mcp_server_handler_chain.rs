//! Test that MCP server doesn't break the handler chain for NewSessionRequest.
//!
//! This is a regression test for a bug where `McpServer::handle_message` would
//! forward `NewSessionRequest` directly to the agent instead of returning
//! `Handled::No`, which prevented downstream `.on_receive_request_from()` handlers
//! from being invoked.

use sacp::link::{AgentToClient, ProxyToConductor};
use sacp::mcp_server::McpServer;
use sacp::schema::{
    AgentCapabilities, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, ProtocolVersion, SessionId,
};
use sacp::{AgentPeer, ClientPeer, Component};
use sacp_conductor::{Conductor, ProxiesAndAgent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Simple echo tool parameters
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoParams {
    message: String,
}

/// Simple echo tool output
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoOutput {
    result: String,
}

/// Test helper to receive a JSON-RPC response
async fn recv<T: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<T>,
) -> Result<T, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.on_receiving_result(async move |result| {
        tx.send(result).map_err(|_| sacp::Error::internal_error())
    })?;
    rx.await.map_err(|_| sacp::Error::internal_error())?
}

/// Tracks whether the NewSessionRequest handler was invoked
struct HandlerConfig {
    new_session_handler_called: AtomicBool,
}

impl HandlerConfig {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            new_session_handler_called: AtomicBool::new(false),
        })
    }

    fn was_handler_called(&self) -> bool {
        self.new_session_handler_called.load(Ordering::SeqCst)
    }
}

/// A proxy component that has BOTH an MCP server AND a NewSessionRequest handler.
/// The bug was that when both were present, the NewSessionRequest handler was never called.
struct ProxyWithMcpAndHandler {
    config: Arc<HandlerConfig>,
}

impl Component<ProxyToConductor> for ProxyWithMcpAndHandler {
    async fn serve(
        self,
        client: impl Component<sacp::link::ConductorToProxy>,
    ) -> Result<(), sacp::Error> {
        let config = Arc::clone(&self.config);

        // Create an MCP server with a simple tool
        let mcp_server = McpServer::builder("test-server".to_string())
            .instructions("A test MCP server")
            .tool_fn_mut(
                "echo",
                "Echoes back the input",
                async |params: EchoParams, _cx| {
                    Ok(EchoOutput {
                        result: format!("Echo: {}", params.message),
                    })
                },
                sacp::tool_fn_mut!(),
            )
            .build();

        ProxyToConductor::builder()
            .name("proxy-with-mcp-and-handler")
            // Add the MCP server
            .with_mcp_server(mcp_server)
            // Add a NewSessionRequest handler - this should be invoked!
            .on_receive_request_from(
                ClientPeer,
                async move |request: NewSessionRequest, request_cx, cx| {
                    // Mark that we were called
                    config
                        .new_session_handler_called
                        .store(true, Ordering::SeqCst);

                    // Forward to agent and relay response
                    cx.send_request_to(AgentPeer, request).on_receiving_result(
                        async move |result| {
                            let response: NewSessionResponse = result?;
                            request_cx.respond(response)
                        },
                    )
                },
                sacp::on_receive_request!(),
            )
            .serve(client)
            .await
    }
}

/// A simple agent that responds to initialization and session requests
struct SimpleAgent;

impl Component<AgentToClient> for SimpleAgent {
    async fn serve(
        self,
        client: impl Component<sacp::link::ClientToAgent>,
    ) -> Result<(), sacp::Error> {
        AgentToClient::builder()
            .name("simple-agent")
            .on_receive_request(
                async |request: InitializeRequest, request_cx, _cx| {
                    request_cx.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                sacp::on_receive_request!(),
            )
            .on_receive_request(
                async |_request: NewSessionRequest, request_cx, _cx| {
                    request_cx.respond(NewSessionResponse::new(SessionId::new(
                        uuid::Uuid::new_v4().to_string(),
                    )))
                },
                sacp::on_receive_request!(),
            )
            .connect_to(client)?
            .serve()
            .await
    }
}

async fn run_test(
    proxies: Vec<sacp::DynComponent<ProxyToConductor>>,
    agent: sacp::DynComponent<AgentToClient>,
    editor_task: impl AsyncFnOnce(sacp::JrConnectionCx<sacp::ClientToAgent>) -> Result<(), sacp::Error>,
) -> Result<(), sacp::Error> {
    let (editor_out, conductor_in) = duplex(1024);
    let (conductor_out, editor_in) = duplex(1024);

    let transport = sacp::ByteStreams::new(editor_out.compat_write(), editor_in.compat());

    sacp::ClientToAgent::builder()
        .name("editor-to-conductor")
        .with_spawned(|_cx| async move {
            Conductor::new_agent(
                "conductor".to_string(),
                ProxiesAndAgent::new(agent).proxies(proxies),
                Default::default(),
            )
            .run(sacp::ByteStreams::new(
                conductor_out.compat_write(),
                conductor_in.compat(),
            ))
            .await
        })
        .run_until(transport, editor_task)
        .await
}

/// Regression test: NewSessionRequest handler should be invoked even when MCP server is present
#[tokio::test]
async fn test_new_session_handler_invoked_with_mcp_server() -> Result<(), sacp::Error> {
    let handler_config = HandlerConfig::new();
    let handler_config_clone = Arc::clone(&handler_config);

    let proxy = sacp::DynComponent::<ProxyToConductor>::new(ProxyWithMcpAndHandler {
        config: handler_config,
    });
    let agent = sacp::DynComponent::<AgentToClient>::new(SimpleAgent);

    run_test(vec![proxy], agent, async |editor_cx| {
        // Initialize first
        let _init_response =
            recv(editor_cx.send_request(InitializeRequest::new(ProtocolVersion::LATEST))).await?;

        // Create a new session - this should trigger the handler in the proxy
        let session_response =
            recv(editor_cx.send_request(NewSessionRequest::new(PathBuf::from("/tmp")))).await?;

        // Verify we got a valid session ID
        assert!(
            !session_response.session_id.0.is_empty(),
            "Should receive a valid session ID"
        );

        Ok::<(), sacp::Error>(())
    })
    .await?;

    // THE KEY ASSERTION: verify the handler was actually called
    assert!(
        handler_config_clone.was_handler_called(),
        "NewSessionRequest handler should be invoked even when MCP server is in the chain. \
         This is a regression - the MCP server was incorrectly forwarding the request directly \
         to the agent instead of letting it flow through the handler chain."
    );

    Ok(())
}
