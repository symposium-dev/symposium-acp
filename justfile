# Build binaries needed for integration tests
prep-tests:
    cargo build -p sacp-conductor -p elizacp
    cargo build -p sacp-test --bin mcp-echo-server --example arrow_proxy

# Run all tests (requires prep-tests first)
test: prep-tests
    cargo test --all --workspace
