//! Integration test for nested conductors with proxy mode.
//!
//! This test verifies that:
//! 1. Conductors can be nested in proxy chains
//! 2. Inner conductor operates in proxy mode and forwards messages correctly
//! 3. Multiple arrow proxies work correctly through nested conductors
//! 4. The '>' prefix is applied multiple times (once per proxy)
//!
//! Chain structure:
//! test-editor -> outer_conductor -> inner_conductor -> eliza
//!                                    ├─ arrow_proxy1
//!                                    └─ arrow_proxy2
//!
//! Expected behavior:
//! - arrow_proxy1 adds first '>' to eliza's response: ">Hello..."
//! - arrow_proxy2 adds second '>' to that: ">>Hello..."
//! - Inner conductor operates in proxy mode, forwarding to eliza
//! - Outer conductor receives the ">>" prefixed response
//!
//! Run `just prep-tests` before running these tests.

use sacp::{Agent, Client, Conductor, DynServe, Serve};
use sacp_conductor::{ConductorImpl, ProxiesAndAgent};
use sacp_test::arrow_proxy::run_arrow_proxy;
use sacp_test::test_binaries::{arrow_proxy_example, conductor_binary, elizacp};
use sacp_tokio::AcpAgent;
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Mock arrow proxy component for testing.
/// Runs the arrow proxy logic in-process instead of spawning a subprocess.
struct MockArrowProxy;

impl Serve<Conductor> for MockArrowProxy {
    async fn serve(
        self,
        client: impl Serve<sacp::Proxy>,
    ) -> Result<(), sacp::Error> {
        run_arrow_proxy(client).await
    }
}

/// Mock Eliza component for testing.
/// Runs the Eliza agent logic in-process instead of spawning a subprocess.
struct MockEliza;

impl Serve<Client> for MockEliza {
    async fn serve(
        self,
        client: impl Serve<Agent>,
    ) -> Result<(), sacp::Error> {
        Serve::<Client>::serve(elizacp::ElizaAgent::new(true), client).await
    }
}

/// Mock inner conductor component for testing.
/// Creates a nested conductor that runs in-process with mock arrow proxies.
struct MockInnerConductor {
    num_arrow_proxies: usize,
}

impl MockInnerConductor {
    fn new(num_arrow_proxies: usize) -> Self {
        Self { num_arrow_proxies }
    }
}

impl Serve<Conductor> for MockInnerConductor {
    async fn serve(
        self,
        client: impl Serve<sacp::Proxy>,
    ) -> Result<(), sacp::Error> {
        // Create mock arrow proxy components for the inner conductor
        // This conductor is ONLY proxies - no actual agent
        // Use Serve::serve instead of .run() to get the Serve<Conductor> impl
        let mut components: Vec<DynServe<Conductor>> = Vec::new();
        for _ in 0..self.num_arrow_proxies {
            components.push(DynServe::new(MockArrowProxy));
        }

        Serve::<Conductor>::serve(
            sacp_conductor::ConductorImpl::new_proxy(
                "inner-conductor".to_string(),
                components,
                Default::default(),
            ),
            client,
        )
        .await
    }
}

#[tokio::test]
async fn test_nested_conductor_with_arrow_proxies() -> Result<(), sacp::Error> {
    // Create the nested component chain using mock components
    // Inner conductor will manage: arrow_proxy1 -> arrow_proxy2 -> eliza
    // Outer conductor will manage: inner_conductor only

    // Create duplex streams for editor <-> conductor communication
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    // Spawn the outer conductor with the inner conductor and eliza
    let conductor_handle = tokio::spawn(async move {
        ConductorImpl::new_agent(
            "outer-conductor".to_string(),
            ProxiesAndAgent::new(MockEliza).proxy(MockInnerConductor::new(2)),
            Default::default(),
        )
        .run(sacp::ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        ))
        .await
    });

    // Wait for editor to complete and get the result
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
        let result = yopo::prompt(
            sacp::ByteStreams::new(editor_write.compat_write(), editor_read.compat()),
            "Hello",
        )
        .await?;

        tracing::debug!(?result, "Received response from nested conductor chain");

        expect_test::expect![[r#"
            ">>How do you do. Please state your problem."
        "#]]
        .assert_debug_eq(&result);

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    tracing::info!(
        ?result,
        "Test completed successfully with double-arrow-prefixed response from nested conductor"
    );

    conductor_handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_nested_conductor_with_external_arrow_proxies() -> Result<(), sacp::Error> {
    // Create the nested component chain using external processes
    // Inner conductor spawned as a separate process with two arrow proxies
    // Outer conductor manages: inner_conductor -> eliza (both as external processes)
    // Uses pre-built binaries to avoid cargo run races during `cargo test --all`
    let conductor_path = conductor_binary().to_string_lossy().to_string();
    let arrow_proxy_path = arrow_proxy_example().to_string_lossy().to_string();
    let inner_conductor = AcpAgent::from_args([
        &conductor_path,
        "proxy",
        &arrow_proxy_path,
        &arrow_proxy_path,
    ])?;
    let eliza = elizacp();

    // Create duplex streams for editor <-> conductor communication
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    // Spawn the outer conductor with the inner conductor and eliza as external processes
    let conductor_handle = tokio::spawn(async move {
        ConductorImpl::new_agent(
            "outer-conductor".to_string(),
            ProxiesAndAgent::new(eliza).proxy(inner_conductor),
            Default::default(),
        )
        .run(sacp::ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        ))
        .await
    });

    // Wait for editor to complete and get the result
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
        let result = yopo::prompt(
            sacp::ByteStreams::new(editor_write.compat_write(), editor_read.compat()),
            "Hello",
        )
        .await?;

        tracing::debug!(?result, "Received response from nested conductor chain");

        expect_test::expect![[r#"
            ">>How do you do. Please state your problem."
        "#]]
        .assert_debug_eq(&result);

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    tracing::info!(
        ?result,
        "Test completed successfully with double-arrow-prefixed response from nested conductor"
    );

    conductor_handle.abort();

    Ok(())
}
