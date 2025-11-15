//! Integration tests for McpServiceRegistry handler chain behavior.
//!
//! These tests verify that the `provide_mcp` handler correctly modifies
//! `NewSessionRequest` messages and allows subsequent handlers to see
//! the modified request with injected MCP servers.

use sacp::schema::{McpServer, NewSessionRequest, NewSessionResponse};
use sacp::{JrHandlerChain, JrRequestCx};
use sacp_proxy::{AcpProxyExt, McpServiceRegistry};
use std::sync::{Arc, Mutex};
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Test helper to receive a response.
async fn recv<R: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<R>,
) -> Result<R, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.await_when_result_received(async move |result| {
        tx.send(result).map_err(|_| sacp::Error::internal_error())
    })?;
    rx.await.map_err(|_| sacp::Error::internal_error())?
}

/// Mock MCP server component for testing
struct MockMcpServer;

impl sacp::Component for MockMcpServer {
    fn serve(
        self,
        client: impl sacp::Component,
    ) -> impl std::future::Future<Output = Result<(), sacp::Error>> + Send {
        async move {
            // Mock server just serves the client directly
            sacp::JrHandlerChain::new().serve(client).await
        }
    }
}

// ============================================================================
// Test 1: provide_mcp modifies request and allows subsequent handlers to see it
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn test_provide_mcp_handler_chain() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // Create registry and add a test MCP server
            let registry = McpServiceRegistry::new();
            registry
                .add_mcp_server("test-server", || MockMcpServer)
                .expect("Failed to add MCP server");

            // Track what the custom handler sees
            let seen_servers = Arc::new(Mutex::new(Vec::new()));
            let seen_servers_clone = seen_servers.clone();

            // Build handler chain: provide_mcp THEN custom handler
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = JrHandlerChain::new()
                .provide_mcp(registry)
                .on_receive_request(
                    async move |request: NewSessionRequest,
                                request_cx: JrRequestCx<NewSessionResponse>| {
                        // Record what MCP servers we see
                        let mut servers = seen_servers_clone.lock().unwrap();
                        for server in &request.mcp_servers {
                            if let McpServer::Http { name, .. } = server {
                                servers.push(name.clone());
                            }
                        }

                        // Respond with the servers we saw
                        request_cx.respond(NewSessionResponse {
                            session_id: "test-session".to_string().into(),
                            meta: None,
                            modes: None,
                        })
                    },
                )
                .proxy();

            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = JrHandlerChain::new();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {e:?}");
                }
            });

            let result = client
                .with_client(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        // Send NewSessionRequest with no MCP servers
                        let request = NewSessionRequest {
                            mcp_servers: vec![],
                            cwd: std::env::current_dir()
                                .unwrap_or_else(|_| std::path::PathBuf::from("/")),
                            meta: None,
                        };

                        let response =
                            recv(cx.send_request(request))
                                .await
                                .map_err(|e| -> sacp::Error {
                                    sacp::util::internal_error(format!(
                                        "NewSession request failed: {e:?}"
                                    ))
                                })?;

                        assert_eq!(response.session_id, "test-session".into());

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify the custom handler saw the MCP server that provide_mcp injected
            let servers = seen_servers.lock().unwrap();
            assert_eq!(
                servers.len(),
                1,
                "Expected custom handler to see 1 MCP server"
            );
            assert_eq!(servers[0], "test-server");
        })
        .await;
}

// ============================================================================
// Test 2: provide_mcp preserves existing MCP servers in request
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn test_provide_mcp_preserves_existing_servers() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // Create registry and add a test MCP server
            let registry = McpServiceRegistry::new();
            registry
                .add_mcp_server("injected-server", || MockMcpServer)
                .expect("Failed to add MCP server");

            // Track what the custom handler sees
            let seen_servers = Arc::new(Mutex::new(Vec::new()));
            let seen_servers_clone = seen_servers.clone();

            // Build handler chain
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = JrHandlerChain::new()
                .provide_mcp(registry)
                .on_receive_request(
                    async move |request: NewSessionRequest,
                                request_cx: JrRequestCx<NewSessionResponse>| {
                        // Record what MCP servers we see
                        let mut servers = seen_servers_clone.lock().unwrap();
                        for server in &request.mcp_servers {
                            if let McpServer::Http { name, .. } = server {
                                servers.push(name.clone());
                            }
                        }

                        request_cx.respond(NewSessionResponse {
                            session_id: "test-session".to_string().into(),
                            meta: None,
                            modes: None,
                        })
                    },
                )
                .proxy();

            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = JrHandlerChain::new();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {e:?}");
                }
            });

            let result = client
                .with_client(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        // Send NewSessionRequest with an existing MCP server
                        let request = NewSessionRequest {
                            mcp_servers: vec![McpServer::Http {
                                name: "existing-server".to_string(),
                                url: "http://example.com".to_string(),
                                headers: vec![],
                            }],
                            cwd: std::env::current_dir()
                                .unwrap_or_else(|_| std::path::PathBuf::from("/")),
                            meta: None,
                        };

                        let response =
                            recv(cx.send_request(request))
                                .await
                                .map_err(|e| -> sacp::Error {
                                    sacp::util::internal_error(format!(
                                        "NewSession request failed: {e:?}"
                                    ))
                                })?;

                        assert_eq!(response.session_id, "test-session".into());

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify the custom handler saw both the existing and injected servers
            let servers = seen_servers.lock().unwrap();
            assert_eq!(
                servers.len(),
                2,
                "Expected custom handler to see 2 MCP servers"
            );
            assert!(servers.contains(&"existing-server".to_string()));
            assert!(servers.contains(&"injected-server".to_string()));
        })
        .await;
}

// ============================================================================
// Test 3: Multiple MCP servers can be registered
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn test_provide_mcp_multiple_servers() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // Create registry and add multiple test MCP servers
            let registry = McpServiceRegistry::new();
            registry
                .add_mcp_server("server-1", || MockMcpServer)
                .expect("Failed to add MCP server 1");
            registry
                .add_mcp_server("server-2", || MockMcpServer)
                .expect("Failed to add MCP server 2");
            registry
                .add_mcp_server("server-3", || MockMcpServer)
                .expect("Failed to add MCP server 3");

            // Track what the custom handler sees
            let seen_servers = Arc::new(Mutex::new(Vec::new()));
            let seen_servers_clone = seen_servers.clone();

            // Build handler chain
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = JrHandlerChain::new()
                .provide_mcp(registry)
                .on_receive_request(
                    async move |request: NewSessionRequest,
                                request_cx: JrRequestCx<NewSessionResponse>| {
                        // Record what MCP servers we see
                        let mut servers = seen_servers_clone.lock().unwrap();
                        for server in &request.mcp_servers {
                            if let McpServer::Http { name, .. } = server {
                                servers.push(name.clone());
                            }
                        }

                        request_cx.respond(NewSessionResponse {
                            session_id: "test-session".to_string().into(),
                            meta: None,
                            modes: None,
                        })
                    },
                )
                .proxy();

            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = JrHandlerChain::new();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {e:?}");
                }
            });

            let result = client
                .with_client(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        let request = NewSessionRequest {
                            mcp_servers: vec![],
                            cwd: std::env::current_dir()
                                .unwrap_or_else(|_| std::path::PathBuf::from("/")),
                            meta: None,
                        };

                        let response =
                            recv(cx.send_request(request))
                                .await
                                .map_err(|e| -> sacp::Error {
                                    sacp::util::internal_error(format!(
                                        "NewSession request failed: {e:?}"
                                    ))
                                })?;

                        assert_eq!(response.session_id, "test-session".into());

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify the custom handler saw all 3 injected servers
            let servers = seen_servers.lock().unwrap();
            assert_eq!(
                servers.len(),
                3,
                "Expected custom handler to see 3 MCP servers"
            );
            assert!(servers.contains(&"server-1".to_string()));
            assert!(servers.contains(&"server-2".to_string()));
            assert!(servers.contains(&"server-3".to_string()));
        })
        .await;
}
