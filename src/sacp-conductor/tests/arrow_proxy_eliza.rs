//! Integration test for conductor with arrow proxy and eliza agent.
//!
//! This test verifies that:
//! 1. Conductor can orchestrate a proxy chain with arrow proxy + eliza
//! 2. Session updates from eliza get the '>' prefix from arrow proxy
//! 3. The full proxy chain works end-to-end
//!
//! Run `just prep-tests` before running this test.

use sacp_conductor::{Conductor, ProxiesAndAgent};
use sacp_test::test_binaries::{arrow_proxy_example, elizacp_binary};
use sacp_tokio::AcpAgent;
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::test]
async fn test_conductor_with_arrow_proxy_and_eliza() -> Result<(), sacp::Error> {
    // Create the component chain: arrow_proxy -> eliza
    // Uses pre-built binaries to avoid cargo run races during `cargo test --all`
    let arrow_proxy_agent =
        AcpAgent::from_args([arrow_proxy_example().to_string_lossy().to_string()])?;
    let eliza_agent = AcpAgent::from_args([elizacp_binary().to_string_lossy().to_string()])?;

    // Create duplex streams for editor <-> conductor communication
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    // Spawn the conductor
    let conductor_handle = tokio::spawn(async move {
        Conductor::new_agent(
            "conductor".to_string(),
            ProxiesAndAgent::new(eliza_agent).proxy(arrow_proxy_agent),
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

        tracing::debug!(?result, "Received response from arrow proxy chain");

        assert!(
            result.starts_with('>'),
            "Expected response to start with '>' from arrow proxy, got: {}",
            result
        );

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    tracing::info!(
        ?result,
        "Test completed successfully with arrow-prefixed response"
    );

    conductor_handle.abort();

    Ok(())
}
