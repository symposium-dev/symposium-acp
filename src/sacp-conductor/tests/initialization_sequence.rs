//! Integration tests for the initialization sequence.
//!
//! These tests verify that:
//! 1. Single-component chains receive `InitializeRequest` (agent mode)
//! 2. Multi-component chains: proxies receive `InitializeProxyRequest`
//! 3. Last component (agent) receives `InitializeRequest`

use sacp::schema::{
    AgentCapabilities, InitializeProxyRequest, InitializeRequest, InitializeResponse,
};
use sacp::{Client, Component, ProxyToConductor};
use sacp_conductor::Conductor;
use std::sync::Arc;
use std::sync::Mutex;

use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Test helper to receive a JSON-RPC response
async fn recv<T: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<T>,
) -> Result<T, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.await_when_result_received(async move |result| {
        tx.send(result).map_err(|_| sacp::Error::internal_error())
    })?;
    rx.await.map_err(|_| sacp::Error::internal_error())?
}

/// Tracks what type of initialization request was received
#[derive(Debug, Clone, PartialEq)]
enum InitRequestType {
    Initialize,
    InitializeProxy,
}

struct InitConfig {
    /// What type of init request was received
    received_init_type: Mutex<Option<InitRequestType>>,
}

impl InitConfig {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            received_init_type: Mutex::new(None),
        })
    }

    fn read_init_type(&self) -> Option<InitRequestType> {
        self.received_init_type
            .lock()
            .expect("not poisoned")
            .clone()
    }
}

struct InitComponent {
    config: Arc<InitConfig>,
}

impl InitComponent {
    fn new(config: &Arc<InitConfig>) -> sacp::DynComponent {
        sacp::DynComponent::new(Self {
            config: config.clone(),
        })
    }
}

impl Component for InitComponent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        let config = Arc::clone(&self.config);
        let config2 = Arc::clone(&self.config);

        {
            ProxyToConductor::builder()
                .name("init-component")
                // Handle InitializeProxyRequest (we're a proxy)
                .on_receive_request_from(
                    Client,
                    async move |request: InitializeProxyRequest, request_cx, cx| {
                        *config.received_init_type.lock().expect("unpoisoned") =
                            Some(InitRequestType::InitializeProxy);

                        // Forward to successor and respond
                        cx.send_request_to(sacp::Agent, request)
                            .await_when_result_received(async move |response| {
                                let response: InitializeResponse = response?;
                                request_cx.respond(response)
                            })
                    },
                )
                // Handle InitializeRequest (we're the agent)
                .on_receive_request_from(
                    Client,
                    async move |request: InitializeRequest, request_cx, _cx| {
                        *config2.received_init_type.lock().expect("unpoisoned") =
                            Some(InitRequestType::Initialize);

                        // We're the final component, just respond
                        let response = InitializeResponse {
                            protocol_version: request.protocol_version,
                            agent_capabilities: AgentCapabilities::default(),
                            auth_methods: vec![],
                            meta: None,
                            agent_info: None,
                        };

                        request_cx.respond(response)
                    },
                )
                .serve(client)
                .await
        }
    }
}

async fn run_test_with_components(
    components: Vec<sacp::DynComponent>,
    editor_task: impl AsyncFnOnce(sacp::JrConnectionCx<sacp::ClientToAgent>) -> Result<(), sacp::Error>,
) -> Result<(), sacp::Error> {
    // Set up editor <-> conductor communication
    let (editor_out, conductor_in) = duplex(1024);
    let (conductor_out, editor_in) = duplex(1024);

    let transport = sacp::ByteStreams::new(editor_out.compat_write(), editor_in.compat());

    sacp::ClientToAgent::builder()
        .name("editor-to-connector")
        .with_spawned(|_cx| async move {
            Conductor::new("conductor".to_string(), components, Default::default())
                .run(sacp::ByteStreams::new(
                    conductor_out.compat_write(),
                    conductor_in.compat(),
                ))
                .await
        })
        .with_client(transport, editor_task)
        .await
}

#[tokio::test]
async fn test_single_component_gets_initialize_request() -> Result<(), sacp::Error> {
    // Single component should receive InitializeRequest (it's the agent)
    let component1 = InitConfig::new();

    run_test_with_components(vec![InitComponent::new(&component1)], async |editor_cx| {
        let init_response = recv(editor_cx.send_request(InitializeRequest {
            protocol_version: Default::default(),
            client_capabilities: Default::default(),
            meta: None,
            client_info: None,
        }))
        .await;

        assert!(
            init_response.is_ok(),
            "Initialize should succeed: {:?}",
            init_response
        );

        Ok::<(), sacp::Error>(())
    })
    .await?;

    // Single component should receive InitializeRequest (not InitializeProxyRequest)
    assert_eq!(
        component1.read_init_type(),
        Some(InitRequestType::Initialize),
        "Single component should receive InitializeRequest"
    );

    Ok(())
}

#[tokio::test]
async fn test_two_components_proxy_gets_initialize_proxy() -> Result<(), sacp::Error> {
    // First component (proxy) gets InitializeProxyRequest
    // Second component (agent) gets InitializeRequest
    let component1 = InitConfig::new();
    let component2 = InitConfig::new();

    run_test_with_components(
        vec![
            InitComponent::new(&component1),
            InitComponent::new(&component2),
        ],
        async |editor_cx| {
            let init_response = recv(editor_cx.send_request(InitializeRequest {
                protocol_version: Default::default(),
                client_capabilities: Default::default(),
                meta: None,
                client_info: None,
            }))
            .await;

            assert!(
                init_response.is_ok(),
                "Initialize should succeed: {:?}",
                init_response
            );

            Ok::<(), sacp::Error>(())
        },
    )
    .await?;

    // First component (proxy) should receive InitializeProxyRequest
    assert_eq!(
        component1.read_init_type(),
        Some(InitRequestType::InitializeProxy),
        "Proxy component should receive InitializeProxyRequest"
    );

    // Second component (agent) should receive InitializeRequest
    assert_eq!(
        component2.read_init_type(),
        Some(InitRequestType::Initialize),
        "Agent component should receive InitializeRequest"
    );

    Ok(())
}

#[tokio::test]
async fn test_three_components_all_proxies_get_initialize_proxy() -> Result<(), sacp::Error> {
    // First two components (proxies) get InitializeProxyRequest
    // Third component (agent) gets InitializeRequest
    let component1 = InitConfig::new();
    let component2 = InitConfig::new();
    let component3 = InitConfig::new();

    run_test_with_components(
        vec![
            InitComponent::new(&component1),
            InitComponent::new(&component2),
            InitComponent::new(&component3),
        ],
        async |editor_cx| {
            let init_response = recv(editor_cx.send_request(InitializeRequest {
                protocol_version: Default::default(),
                client_capabilities: Default::default(),
                meta: None,
                client_info: None,
            }))
            .await;

            assert!(
                init_response.is_ok(),
                "Initialize should succeed: {:?}",
                init_response
            );

            Ok::<(), sacp::Error>(())
        },
    )
    .await?;

    // First two components (proxies) should receive InitializeProxyRequest
    assert_eq!(
        component1.read_init_type(),
        Some(InitRequestType::InitializeProxy),
        "First proxy should receive InitializeProxyRequest"
    );
    assert_eq!(
        component2.read_init_type(),
        Some(InitRequestType::InitializeProxy),
        "Second proxy should receive InitializeProxyRequest"
    );

    // Third component (agent) should receive InitializeRequest
    assert_eq!(
        component3.read_init_type(),
        Some(InitRequestType::Initialize),
        "Agent component should receive InitializeRequest"
    );

    Ok(())
}
