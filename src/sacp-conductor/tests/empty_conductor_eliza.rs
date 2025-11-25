//! Integration test for conductor with an empty conductor and eliza agent.
//!
//! This test verifies that:
//! 1. Conductor can orchestrate a chain with an empty conductor as a proxy + eliza
//! 2. Empty conductor (with no components) correctly acts as a passthrough proxy
//! 3. Messages flow correctly through the empty conductor to eliza
//! 4. The full chain works end-to-end

use sacp::Component;
use sacp_conductor::Conductor;
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Mock empty conductor component for testing.
/// Creates a nested conductor with no components that acts as a passthrough proxy.
struct MockEmptyConductor;

impl Component for MockEmptyConductor {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Create an empty conductor with no components - it should act as a passthrough
        let empty_components: Vec<sacp::DynComponent> = vec![];
        Conductor::new("empty-conductor".to_string(), empty_components, Default::default())
            .run(client)
            .await
    }
}

/// Mock Eliza component for testing.
/// Runs the Eliza agent logic in-process instead of spawning a subprocess.
struct MockEliza;

impl Component for MockEliza {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        elizacp::ElizaAgent::new().serve(client).await
    }
}

#[tokio::test]
async fn test_conductor_with_empty_conductor_and_eliza() -> Result<(), sacp::Error> {
    // Initialize tracing for debugging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace")),
        )
        .with_test_writer()
        .try_init();
    // Create the component chain: empty_conductor -> eliza
    let empty_conductor = sacp::DynComponent::new(MockEmptyConductor);
    let eliza = sacp::DynComponent::new(MockEliza);

    // Create duplex streams for editor <-> conductor communication
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    // Spawn the conductor
    let conductor_handle = tokio::spawn(async move {
        Conductor::new(
            "outer-conductor".to_string(),
            vec![empty_conductor, eliza],
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

        tracing::debug!(?result, "Received response from empty conductor chain");

        // Empty conductor should not modify the response, so we expect
        // the standard eliza response without any prefix
        expect_test::expect![[r#"
            "Hello. How are you feeling today?"
        "#]]
        .assert_debug_eq(&result);

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    tracing::info!(
        ?result,
        "Test completed successfully with response from empty conductor chain"
    );

    conductor_handle.abort();

    Ok(())
}
