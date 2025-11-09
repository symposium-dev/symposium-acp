# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0-alpha.3](https://github.com/symposium-dev/symposium-acp/compare/sacp-conductor-v1.0.0-alpha.2...sacp-conductor-v1.0.0-alpha.3) - 2025-11-09

### Added

- *(sacp-conductor)* implement lazy component initialization
- *(sacp-conductor)* add ComponentList trait for lazy component instantiation

### Fixed

- *(sacp-conductor)* prevent response messages from overtaking notifications

### Other

- Merge pull request #18 from nikomatsakis/main
- *(sacp-conductor)* document lazy initialization and ComponentList trait
- *(sacp-conductor)* use if-let and assert in lazy initialization
- *(sacp-conductor)* route all message forwarding through central queue
- *(sacp-conductor)* decouple message handler from component count

## [1.0.0-alpha.2](https://github.com/symposium-dev/symposium-acp/compare/sacp-conductor-v1.0.0-alpha.1...sacp-conductor-v1.0.0-alpha.2) - 2025-11-08

### Added

- *(sacp)* add convenience methods for common connection patterns

### Other

- fix doctests for API refactoring
- wip wip wip
- wipwipwip
- [**breaking**] remove Unpin bounds and simplify transport API

## [0.2.0](https://github.com/symposium-dev/symposium-acp/compare/sacp-conductor-v0.1.1...sacp-conductor-v0.2.0) - 2025-11-04

### Fixed

- fix github url

### Other

- *(sacp-conductor)* add comprehensive crate-level documentation
- *(sacp)* [**breaking**] rename json_rpc_cx to connection_cx
- rename JsonRpcRequest to JrRequest
- add READMEs for sacp-tokio, sacp-proxy, and sacp-conductor

## [0.1.1](https://github.com/symposium-dev/symposium-acp/compare/sacp-conductor-v0.1.0...sacp-conductor-v0.1.1) - 2025-10-30

### Fixed

- replace crate::Error with sacp::Error in dependent crates

### Other

- remove more uses of `agent_client_protocl_schema`
- replace acp:: with crate::/sacp:: throughout codebase
- rename JsonRpc* types to Jr* across all crates
- *(deps)* switch from agent-client-protocol to agent-client-protocol-schema
- prepare release-plz
