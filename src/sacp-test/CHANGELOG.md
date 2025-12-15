# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [6.0.0](https://github.com/symposium-dev/symposium-acp/releases/tag/sacp-test-v6.0.0) - 2025-12-15

### Added

- [**breaking**] introduce role-based connection API
- [**breaking**] change JrMessage trait to take &self and require Clone
- *(sacp-test)* add mcp-echo-server binary for testing
- *(sacp)* add IntoHandled trait for flexible handler return types
- *(sacp-test)* add arrow proxy for testing

### Fixed

- fix cargo.toml metadata, dang it

### Other

- set all crates to version 6.0.0
- release
- cleanup cargo metadata
- replace yolo_prompt with direct yopo::prompt calls
- *(yopo)* return sacp::Error instead of Box<dyn Error>
- *(sacp-test)* use yopo library for test client implementation
- release version 1.0.0 for all crates (sacp-rmcp at 0.8.0)
- Revert to state before 1.0.0 release
- release version 1.0.0 for all crates
- *(sacp)* add Component::serve() and simplify channel API
- [**breaking**] make Component trait ergonomic with async fn and introduce DynComponent
- [**breaking**] make Component the primary trait with Transport as blanket impl
- cleanup and simplify some of the logic to avoid "indirection" through
- unify Transport and Component traits with BoxFuture-returning signatures
- create selective jsonrpcmsg re-export module
- replace jsonrpcmsg::Message with sacp::JsonRpcMessage throughout codebase
- Merge pull request #16 from nikomatsakis/main
- fix doctests for API refactoring
- wip wip wip
- [**breaking**] remove Unpin bounds and simplify transport API
- update all versions from 1.0.0-alpha to 1.0.0-alpha.1
- release v1.0.0-alpha
- *(conductor)* add integration test with arrow proxy and eliza
- *(conductor)* add integration test with arrow proxy and eliza
- rename sacp-doc-test to sacp-test

## [1.0.0](https://github.com/symposium-dev/symposium-acp/releases/tag/sacp-test-v1.0.0) - 2025-11-05

### Added

- *(sacp-test)* add arrow proxy for testing

### Other

- update all versions from 1.0.0-alpha to 1.0.0-alpha.1
- release v1.0.0-alpha
- *(conductor)* add integration test with arrow proxy and eliza
- *(conductor)* add integration test with arrow proxy and eliza
- rename sacp-doc-test to sacp-test

## [1.0.0-alpha](https://github.com/symposium-dev/symposium-acp/releases/tag/sacp-test-v1.0.0-alpha) - 2025-11-05

### Added

- *(sacp-test)* add arrow proxy for testing

### Other

- *(conductor)* add integration test with arrow proxy and eliza
- *(conductor)* add integration test with arrow proxy and eliza
- rename sacp-doc-test to sacp-test

## [0.1.0](https://github.com/symposium-dev/symposium-acp/releases/tag/sacp-doc-test-v0.1.0) - 2025-11-04

### Other

- factor "doc-test-only" code into its own crate
