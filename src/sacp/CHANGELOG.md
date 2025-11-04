# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/symposium-dev/symposium-acp/compare/sacp-v0.1.1...sacp-v0.2.0) - 2025-11-04

### Added

- *(sacp-tokio)* implement JrConnectionExt trait for to_agent
- create sacp-tokio crate and improve AcpAgent API
- *(sacp)* add AcpAgent utility and yolo-one-shot-client example

### Fixed

- fix github url

### Other

- add GitHub links to example files in sacp and sacp-proxy
- *(sacp)* [**breaking**] rename json_rpc_cx to connection_cx
- add deny(missing_docs) and document all public APIs
- *(sacp)* improve cx cloning pattern in doc examples
- *(sacp)* update crate-level documentation to match README
- factor "doc-test-only" code into its own crate
- make doctests build
- *(sacp)* fix remaining doc test compilation errors
- *(sacp)* enhance JrResponse method documentation
- *(sacp)* document event loop and concurrency model
- rename JsonRpcRequest to JrRequest
- use util.rs
- *(sacp)* move typed utilities to util module and add docs
- *(sacp)* remove mention of non-existent derive macro
- *(sacp)* fix doctests to compile instead of being ignored
- *(sacp)* add comprehensive rustdoc for JrConnection
- *(sacp)* rewrite README with streamlined quick start
- *(sacp)* use stderr for yolo-one-shot-client meta output

## [0.1.1](https://github.com/symposium-dev/symposium-acp/compare/sacp-v0.1.0...sacp-v0.1.1) - 2025-10-30

### Added

- *(sacp)* re-export all agent-client-protocol-schema types

### Fixed

- replace crate::Error with sacp::Error in dependent crates

### Other

- remove more uses of `agent_client_protocl_schema`
- replace acp:: with crate::/sacp:: throughout codebase
- rename JsonRpc* types to Jr* across all crates
- *(deps)* switch from agent-client-protocol to agent-client-protocol-schema
- *(sacp)* add simple_agent example demonstrating JsonRpcConnection usage
