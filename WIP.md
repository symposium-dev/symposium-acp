# WIP: Role-Based API Refactoring

## Overview

Refactoring the Link/Peer type system to a simpler Role-based API. Goals:
- Eliminate `Jr` and `Cx` from type names
- Replace Link types (which encode both sides) with Role types (which encode one side)
- Simplify the builder/connection API
- Move conductor-specific types to sacp-conductor crate

## New Type Mapping

| Old | New |
|-----|-----|
| `JrConnectionBuilder<AgentToClient>` | `ConnectFrom<Agent>` |
| `JrConnection<AgentToClient>` | *(removed)* |
| `JrConnectionCx<AgentToClient>` | `ConnectionTo<Client>` |
| `JrRequestCx<R>` | `Responder<R>` |
| `JrResponseCx<R>` | `ResponseRouter<R>` |

## New API Shape

```rust
// Purely reactive - just handle incoming messages
Agent::builder()
    .on_receive_request(...)
    .serve(transport)
    .await

// Active connection - drive the interaction
Client::builder()
    .connect_to(transport, async |connection: ConnectionTo<Agent>| {
        connection.send_request(...).await
    })
    .await
```

## Role Traits

```rust
/// The role that an endpoint can play
trait Role {
    /// The role that this endpoint connects to.
    /// For Agent, this is Client.
    /// For Client, this is Agent.
    /// For Proxy, this is Conductor.
    type Counterpart: Role;
    
    fn default_handler();
}

/// A role P *has a peer* Q if P can send/receive messages from Q.
///
/// - `Client: HasPeer<Agent>`
/// - `Agent: HasPeer<Client>`
/// - `Proxy: HasPeer<Client>`
/// - `Proxy: HasPeer<Agent>`
///
/// Note that nobody has Conductor as their peer.
/// Even though a proxy connects to the conductor,
/// it can't logically send messages to the conductor.
trait HasPeer<Peer: Role>: Role {
    fn request_style();
}
```

## Core Roles (in sacp crate)

- `Agent` - counterpart is `Client`
- `Client` - counterpart is `Agent`
- `Proxy` - counterpart is `Conductor`

## Conductor Types (in sacp-conductor crate)

### External presentation

```rust
struct ConductorImpl<R: ConductorRole> { }
trait ConductorRole {}
```

- `ConductorImpl<Agent>` - conductor presenting as agent to clients (was `ConductorToClient`)
- `ConductorImpl<Proxy>` - conductor presenting as proxy to other conductors (was `ConductorToConductor`)

### Internal connections

The conductor uses standard connection types with custom handlers:

- `ConnectionTo<Agent>` - conductor talking to managed agent
- `ConnectionTo<Proxy>` - conductor talking to proxy in chain

No special types needed. Handlers intercept all messages so default handler never fires.

### Types that go away

- `ConductorToAgent` - becomes `ConnectionTo<Agent>` with custom handlers
- `ConductorToProxy` - becomes `ConnectionTo<Proxy>` with custom handlers

## Public API (from sacp crate)

These stay public because external code needs them:
- `ConductorImpl<Agent>` (was `ConductorToClient`)
- `ConductorImpl<Proxy>` (was `ConductorToConductor`)

## Migration Phases

### Phase 1: Introduce Role traits
- [ ] Define `Role` trait with `Counterpart` associated type
- [ ] Define `HasPeer<Peer>` trait
- [ ] Implement for `Agent`, `Client`, `Proxy`

### Phase 2: Rename context types
- [ ] `JrConnectionCx` → `ConnectionTo`
- [ ] `JrRequestCx` → `Responder`
- [ ] `JrResponseCx` → `ResponseRouter`

### Phase 3: Simplify builder API
- [ ] Remove `JrConnection` intermediate type
- [ ] `connect_to(transport)?.run_until(...)` → `connect_to(transport, |cx| ...)`
- [ ] Keep `serve(transport)` for reactive case

### Phase 4: Rename builder
- [ ] `JrConnectionBuilder` → `ConnectFrom`

### Phase 5: Migrate conductor types
- [ ] Move `ConductorToAgent`, `ConductorToProxy` into sacp-conductor as private
- [ ] Replace with `ConnectionTo<Agent>`, `ConnectionTo<Proxy>` + custom handlers
- [ ] Create `ConductorImpl<R>` for external presentation

### Phase 6: Remove Link types
- [ ] Replace Link type parameters with Role type parameters
- [ ] Remove old Link types (`AgentToClient`, etc.)
- [ ] Update `peer_id` to `role_id`

## Open Questions

- Exact module structure for Role types in sacp
- Whether `Conductor` itself is a Role or just appears in `ConductorImpl<R>`
