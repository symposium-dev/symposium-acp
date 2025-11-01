# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/symposium-dev/symposium-acp/compare/sacp-proxy-v0.1.1...sacp-proxy-v0.1.2) - 2025-11-01

### Other

- update Cargo.toml dependencies

## [0.1.1](https://github.com/symposium-dev/symposium-acp/compare/sacp-proxy-v0.1.0...sacp-proxy-v0.1.1) - 2025-10-30

### Fixed

- replace crate::Error with sacp::Error in dependent crates

### Other

- remove more uses of `agent_client_protocl_schema`
- replace acp:: with crate::/sacp:: throughout codebase
- rename JsonRpc* types to Jr* across all crates
- *(deps)* switch from agent-client-protocol to agent-client-protocol-schema
