# WIP: Role-Based API Refactoring

## Overview

Refactoring the Link/Peer type system to a simpler Role-based API. Goals:
- Eliminate `Jr` and `Cx` from type names
- Replace Link types (which encode both sides) with Role types (which encode one side)
- Simplify the builder/connection API
- Move conductor-specific types to sacp-conductor crate

## Current State (2026-01-19)

**Branch**: `the-big-rename`
**Status**: Phase 6 complete, all tests passing

The Role-based API migration is complete. The codebase now uses a unified `Role` type system instead of the previous `JrLink`/`JrPeer` system.

## Type Mapping (Final)

| Old | New |
|-----|-----|
| `Component<L>` | `Serve<R>` |
| `DynComponent<L>` | `DynServe<R>` |
| `ClientToAgent` | `Client` |
| `AgentToClient` | `Agent` |
| `ProxyToConductor` | `Proxy` / `Conductor` |
| `AgentPeer`, `ClientPeer` | `Agent`, `Client` (unified) |
| `Conductor` (struct) | `ConductorImpl` |
| `Run` (trait) | `RunWithConnectionTo` |
| `JrLink` | Removed |
| `JrPeer` | Removed |

## New API Shape

```rust
// Purely reactive - just handle incoming messages
Agent::builder()
    .on_receive_request(...)
    .serve(transport)
    .await

// Active connection - drive the interaction
Client::builder()
    .run_until(transport, async |connection: ConnectionTo<Agent>| {
        connection.send_request(...).await
    })
    .await
```

## Role Traits

```rust
/// The role that an endpoint can play
trait Role {
    /// The role that this endpoint connects to.
    type Counterpart: Role<Counterpart = Self>;

    fn role_id(&self) -> RoleId;
    fn counterpart(&self) -> Self::Counterpart;
    fn default_handle_message_from(...);
}

/// A role P *has a peer* Q if P can send/receive messages from Q.
trait HasPeer<Peer: Role>: Role {
    fn remote_style(&self, peer: Peer) -> RemoteStyle;
}
```

## Module Structure

- `sacp::role` - Role trait and utilities
- `sacp::role::acp` - ACP roles: `Client`, `Agent`, `Proxy`, `Conductor`
- `sacp::role::mcp` - MCP roles for MCP server/client connections

## Migration Phases

### Phase 1: Introduce Role traits ✅
- [x] Define `Role` trait with `Counterpart` associated type
- [x] Define `HasPeer<Peer>` trait
- [x] Implement for `Agent`, `Client`, `Proxy`, `Conductor`

### Phase 2: Rename JrResponder ecosystem to Run ✅
- [x] `JrResponder` → `Run` (now `RunWithConnectionTo`)
- [x] `NullResponder` → `NullRun`
- [x] `ChainResponder` → `ChainRun`
- [x] `SpawnedResponder` → `SpawnedRun`

### Phase 3: Rename context types ✅
- [x] `JrConnectionCx` → `ConnectionTo`
- [x] `JrRequestCx` → `Responder`
- [x] `JrResponseCx` → `ResponseRouter`

### Phase 4: Simplify builder API ✅
- [x] Simplify `spawn_connection` to take `(builder, transport)` instead of `(connection, serve_fn)`
- [x] Make `JrConnection` internal (not publicly exported)
- [x] Keep convenience methods `serve(transport)` and `run_until(transport, main_fn)` on builder

### Phase 5: Rename builder ✅
- [x] `JrConnectionBuilder` → `ConnectFrom`

### Phase 6: Replace Link and Peer types with Role types ✅
- [x] Migrate `HandleMessageFrom` to use `Role` type parameter
- [x] Migrate `ConnectionTo` to use `Role` (counterpart's role)
- [x] Add `builder()` method to Role types (`Client::builder()`, `Agent::builder()`)
- [x] Delete `JrPeer` types - unified with Role types
- [x] Rename `Component<L>` → `Serve<R>`
- [x] Rename `DynComponent<L>` → `DynServe<R>`
- [x] Update conductor: `Conductor` → `ConductorImpl`
- [x] Update all crates: sacp, sacp-conductor, sacp-tokio, sacp-test, sacp-cookbook, sacp-rmcp, elizacp, yopo
- [x] Migrate all test files
- [x] Remove `sacp-tee` crate (no longer maintained)

### Phase 7: Documentation ⏳
- [ ] Update sacp crate doctests (currently failing)
- [ ] Update examples in documentation
- [ ] Review and update mdbook docs

## Files Removed

- `src/sacp/src/peer.rs` - JrPeer system removed
- `src/sacp/src/mcp.rs` - MCP declarations moved to `role/mcp.rs`
- `src/sacp-tee/` - Entire crate removed

## Key Patterns Discovered

- `ConductorImpl.run_until(transport)` accepts `Serve<Client>` components directly
- For session-hosted MCP servers: `make_mcp_server::<Agent>()`
- For proxy-hosted MCP servers: `make_mcp_server::<Conductor>()`
- `Serve<R>` means "I serve someone playing role R" - an agent implements `Serve<Client>`
