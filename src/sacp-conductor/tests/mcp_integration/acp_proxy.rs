//! Proxy component that provides MCP tools over ACP transport
//!
//! This proxy provides an MCP server with an acp: URL and verifies that
//! it receives the session_id in the _mcp/connect request.

use sacp::schema::{McpServer, NewSessionRequest, NewSessionResponse};
use sacp::{Component, JrHandlerChain, JrRequestCx};
use sacp_proxy::{AcpProxyExt, McpConnectRequest};
use std::sync::{Arc, Mutex};

/// Shared state to track MCP connections
#[derive(Clone, Default)]
pub struct AcpProxyState {
    /// Tracks session_ids received in _mcp/connect requests
    pub received_session_ids: Arc<Mutex<Vec<String>>>,
}

pub struct AcpProxyComponent {
    state: AcpProxyState,
}

impl AcpProxyComponent {
    pub fn new(state: AcpProxyState) -> Self {
        Self { state }
    }
}

impl Component for AcpProxyComponent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        JrHandlerChain::new()
            .name("acp-proxy-component")
            .on_receive_request({
                let _state = self.state.clone();
                async move |request: NewSessionRequest,
                            request_cx: JrRequestCx<NewSessionResponse>| {
                    // Inject an MCP server with acp: transport
                    let mut modified_request = request;
                    modified_request.mcp_servers.push(McpServer::Http {
                        name: "test-acp-server".to_string(),
                        url: "acp:test-uuid-12345".to_string(),
                        headers: vec![],
                    });

                    // Forward the modified request and get response
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    request_cx
                        .connection_cx()
                        .send_request(modified_request)
                        .await_when_result_received(async move |result| {
                            let _ = tx.send(result);
                            Ok(())
                        })?;

                    let response = rx.await.map_err(|_| sacp::Error::internal_error())??;
                    request_cx.respond(response)
                }
            })
            .on_receive_request({
                let state = self.state.clone();
                async move |request: McpConnectRequest, request_cx| {
                    tracing::info!(
                        acp_url = %request.acp_url,
                        session_id = %request.session_id.0,
                        "Received _mcp/connect with session_id"
                    );

                    // Store the session_id for verification
                    state
                        .received_session_ids
                        .lock()
                        .unwrap()
                        .push(request.session_id.0.to_string());

                    // Return a connection_id
                    request_cx.respond(sacp_proxy::McpConnectResponse {
                        connection_id: "test-connection-123".to_string(),
                    })
                }
            })
            .proxy()
            .serve(client)
            .await
    }
}

pub fn create(state: AcpProxyState) -> sacp::DynComponent {
    sacp::DynComponent::new(AcpProxyComponent::new(state))
}
