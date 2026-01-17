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
| `ConnectFrom<AgentToClient>` | `ConnectFrom<Agent>` |
| `JrConnection<AgentToClient>` | *(removed)* |
| `JrConnectionCx<AgentToClient>` | `ConnectionTo<Client>` |
| `JrRequestCx<R>` | `Responder<R>` |
| `JrResponseCx<R>` | `ResponseRouter<R>` |
| `JrResponder` | `Run` |
| `NullResponder` | `NullRun` |
| `ChainResponder` | `ChainRun` |
| `SpawnedResponder` | `SpawnedRun` |

## New API Shape

```rust
// Purely reactive - just handle incoming messages
Agent::builder()
    .on_receive_request(...)
    .serve(transport)
    .await

// Active connection - drive the interaction
Client::connect_to(transport, async |connection: ConnectionTo<Agent>| {
    connection.send_request(...).await
})
.await

// Equivalent to:
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

### Phase 1: Introduce Role traits ✅
- [x] Define `Role` trait with `Counterpart` associated type
- [x] Define `HasPeer<Peer>` trait
- [x] Implement for `Agent`, `Client`, `Proxy`, `Conductor`

### Phase 2: Rename JrResponder ecosystem to Run ✅
- [x] `JrResponder` → `Run`
- [x] `NullResponder` → `NullRun`
- [x] `ChainResponder` → `ChainRun`
- [x] `SpawnedResponder` → `SpawnedRun`
- [x] Renamed `jsonrpc/responder.rs` → `jsonrpc/run.rs`

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

### Phase 6: Replace Link and Peer types with Role types

**Goal**: Replace `JrLink` and `JrPeer` type systems with unified `Role` types.

**Key mappings**:
| Current | New |
|---------|-----|
| `ConnectionTo<ClientToAgent>` | `ConnectionTo<Agent>` |
| `ConnectionTo<AgentToClient>` | `ConnectionTo<Client>` |
| `AgentToClient::builder()` | `Agent::builder()` |
| `Component<AgentToClient>` | `Component<Agent>` |
| `send_request_to(AgentPeer, msg)` | `send_request_to(Agent, msg)` |
| `JrMessageHandler::type Link` | `JrMessageHandler::type Role` |
| `Link: HasPeer<AgentPeer>` | `R: HasPeer<Agent>` |

**Semantics**:
- `ConnectionTo<R>` = "connection TO someone playing role R" (the counterpart)
- `R::builder()` = "I am role R, building my side of the connection"
- `Component<R>` = "I implement role R"

**HasDefaultPeer removal**:
- `HasDefaultPeer` trait is removed entirely
- Methods like `send_request()` (no `_to`) require `MyRole: HasPeer<MyRole::Counterpart>`
- This is true for `Client` and `Agent` (they can send to their counterpart)
- This is NOT true for `Proxy` - `Proxy::Counterpart = Conductor`, but `Proxy` doesn't implement `HasPeer<Conductor>`
- Result: Proxies must use explicit `send_request_to(Agent, msg)` or `send_request_to(Client, msg)`
- The type system enforces that proxies be explicit about their target

**Implementation steps**:

#### Step 0: Rename JrMessageHandler → HandleMessageFrom ✅
- [x] Rename `JrMessageHandler` → `HandleMessageFrom`
- [x] This is a simple rename before the structural changes
- [x] The name reflects the semantics: "handle messages from role R"

#### Step 1: Extend Role trait
- [ ] Add `type State: Default + Send = ()` to Role trait
- [ ] Add `fn builder()` method to Role trait
- [ ] Ensure `HasPeer<Peer: Role>` has `remote_style()` method

#### Step 2: Migrate ConnectionTo
- [ ] Change `ConnectionTo<Link: JrLink>` → `ConnectionTo<R: Role>`
- [ ] `R` is the **counterpart's role** (who you're talking to)
- [ ] Update `send_request_to<Peer: JrPeer>` → `send_request_to<Peer: Role>`
- [ ] Update bounds from `Link: HasPeer<Peer>` → `R::Counterpart: HasPeer<Peer>`

#### Step 3: Migrate JrMessageHandler
- [ ] Change `type Link: JrLink` → `type Role: Role`
- [ ] Update all handler implementations in `jsonrpc/handlers.rs`

#### Step 4: Migrate ConnectFrom
- [ ] Change handler bound to use `H::Role` instead of `H::Link`

#### Step 5: Rename and migrate Component → Serve
- [ ] Rename `Component<L>` → `Serve<R>`
- [ ] `R` is the **counterpart's role** (who I serve)
- [ ] `serve(self, client: impl Serve<R::Counterpart>)`
- [ ] Example: `impl Serve<Client> for ConnectFrom<Agent>` - an agent builder can serve clients

#### Step 6: Update conductor (keep Link types internal)
- [ ] Keep conductor-specific link types as internal implementation details
- [ ] Update `ConductorLink` trait to bridge with roles

#### Step 7: Remove old types
- [ ] Delete `JrPeer` trait and peer types (`ClientPeer`, `AgentPeer`, etc.)
- [ ] Delete public Link types (`ClientToAgent`, `AgentToClient`, etc.)
- [ ] Keep conductor-internal links for now
- [ ] Update `lib.rs` exports

#### Step 8: Update all usage sites
- [ ] sacp crate internals
- [ ] sacp-conductor
- [ ] sacp-test
- [ ] sacp-cookbook examples
- [ ] All test files

### Phase 7: Migrate conductor types (after Phase 6)
- [ ] Move `ConductorToAgent`, `ConductorToProxy` into sacp-conductor as private
- [ ] Replace with `ConnectionTo<Agent>`, `ConnectionTo<Proxy>` + custom handlers
- [ ] Create `ConductorImpl<R>` for external presentation

## Open Questions

- Exact module structure for Role types in sacp
- Whether `Conductor` itself is a Role or just appears in `ConductorImpl<R>`
