# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.3.0](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.2.1...yopo-v1.3.0) - 2025-11-29

### Added

- *(yopo)* accept multiple command arguments with env var support

## [1.2.1](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.2.0...yopo-v1.2.1) - 2025-11-25

### Other

- updated the following local packages: sacp, sacp-tokio

## [1.2.0](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.1.0...yopo-v1.2.0) - 2025-11-22

### Added

- *(yopo)* add tracing support with configurable log levels
- *(sacp)* add IntoHandled support to on_receive_message

## [1.1.0](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0...yopo-v1.1.0) - 2025-11-18

### Added

- *(yopo)* add library API with callback support for testing SACP agents

### Fixed

- *(yopo)* correct doctest examples to use valid AcpAgent API

### Other

- *(yopo)* simplify callback implementation using AsyncFnMut
- *(yopo)* accept impl ToString for prompt parameter
- *(yopo)* return sacp::Error instead of Box<dyn Error>

## [1.0.0](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.8...yopo-v1.0.0) - 2025-11-13

### Other

- updated the following local packages: sacp, sacp-tokio

## [1.0.0-alpha.8](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.7...yopo-v1.0.0-alpha.8) - 2025-11-12

### Other

- updated the following local packages: sacp, sacp-tokio

## [1.0.0-alpha.7](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.6...yopo-v1.0.0-alpha.7) - 2025-11-12

### Other

- updated the following local packages: sacp, sacp-tokio

## [1.0.0-alpha.6](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.5...yopo-v1.0.0-alpha.6) - 2025-11-11

### Other

- updated the following local packages: sacp, sacp-tokio

## [1.0.0-alpha.5](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.4...yopo-v1.0.0-alpha.5) - 2025-11-11

### Other

- updated the following local packages: sacp-tokio

## [1.0.0-alpha.4](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.3...yopo-v1.0.0-alpha.4) - 2025-11-11

### Other

- updated the following local packages: sacp, sacp-tokio

## [1.0.0-alpha.3](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.2...yopo-v1.0.0-alpha.3) - 2025-11-09

### Other

- updated the following local packages: sacp, sacp-tokio

## [1.0.0-alpha.2](https://github.com/symposium-dev/symposium-acp/compare/yopo-v1.0.0-alpha.1...yopo-v1.0.0-alpha.2) - 2025-11-08

### Other

- Merge pull request #16 from nikomatsakis/main
- wip wip wip
- [**breaking**] remove Unpin bounds and simplify transport API

## [1.0.0-alpha.1](https://github.com/symposium-dev/symposium-acp/releases/tag/yopo-v1.0.0-alpha.1) - 2025-11-05

### Added

- *(yopo)* add yopo binary crate and simplify example

### Other

- Merge pull request #14 from nikomatsakis/main
- update all versions from 1.0.0-alpha to 1.0.0-alpha.1

## [1.0.0-alpha](https://github.com/symposium-dev/symposium-acp/releases/tag/yopo-v1.0.0-alpha) - 2025-11-05

### Added

- *(yopo)* add yopo binary crate and simplify example
