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

use sacp_conductor::conductor::Conductor;
use sacp_test::test_client::yolo_prompt;
use sacp_tokio::AcpAgent;
use std::str::FromStr;
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::test]
async fn test_nested_conductor_with_arrow_proxies() -> Result<(), sacp::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("sacp_conductor=debug".parse().unwrap())
                .add_directive("arrow_proxy=debug".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();

    // Create the nested component chain
    // Inner conductor will manage: arrow_proxy1 -> arrow_proxy2
    // Outer conductor will manage: inner_conductor -> eliza
    let conductor_proxy = AcpAgent::from_str(
        "cargo run -p sacp-conductor -- agent 'cargo run -p sacp-test --example arrow_proxy' 'cargo run -p sacp-test --example arrow_proxy'",
    )?;
    let eliza = AcpAgent::from_str("cargo run -p elizacp")?;

    // Create duplex streams for editor <-> conductor communication
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    // Spawn the outer conductor with two components
    let conductor_handle = tokio::spawn(async move {
        Conductor::run(
            conductor_write.compat_write(),
            conductor_read.compat(),
            vec![Box::new(conductor_proxy), Box::new(eliza)],
        )
        .await
    });

    // Editor side: connect and send a prompt using helper
    let editor_handle = tokio::spawn(async move {
        let result =
            yolo_prompt(editor_write.compat_write(), editor_read.compat(), "Hello").await?;

        tracing::debug!(?result, "Received response from nested conductor chain");

        assert!(
            result.starts_with(">>"),
            "Expected response to start with '>>' from two arrow proxies via nested conductor, got: {}",
            result
        );

        Ok::<String, sacp::Error>(result)
    });

    // Wait for editor to complete and get the result
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), editor_handle)
        .await
        .expect("Test timed out")
        .expect("Editor task panicked")
        .expect("Editor failed");

    tracing::info!(
        ?result,
        "Test completed successfully with double-arrow-prefixed response from nested conductor"
    );

    conductor_handle.abort();

    Ok(())
}
