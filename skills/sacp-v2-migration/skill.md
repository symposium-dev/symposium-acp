---
name: sacp-v2-migration
description: Migrate Rust code from sacp v1.x to v2.0 role-based API. Use when upgrading sacp dependencies, fixing ConnectFrom errors, or when code references MessageAndCx or old connection patterns.
---

# SACP v2 Migration

## Instructions

Apply these transformations when migrating from sacp v1.x to v2.0:

### 1. Replace JrHandlerChain with role-based connection builder

```rust
// Before
JrHandlerChain::new()

// After - choose based on what you're building:
Agent.connect_from()     // agent serving a client
Client.connect_from()     // client connecting to agent
UntypedRole.connect_from()       // tests or dynamic scenarios
```

### 2. Add connection_cx to all handlers

```rust
// Before
.on_receive_request(async move |req: InitializeRequest, request_cx| {

// After
.on_receive_request(async move |req: InitializeRequest, request_cx, _cx| {
```

### 3. Update on_receive_message

```rust
// Before
.on_receive_message(async move |message: MessageAndCx<UntypedMessage, UntypedMessage>| {
    message.respond_with_error(error)
})

// After
.on_receive_message(async move |message: MessageCx, cx: JrConnectionCx<AgentToClient>| {
    message.respond_with_error(error, cx)
})
```

### 4. Parameterize JrConnectionCx

```rust
// Before
|cx: sacp::JrConnectionCx|

// After
|cx: sacp::JrConnectionCx<ClientToAgent>|
```

### 5. Use cx directly for notifications

```rust
// Before
request_cx.connection_cx().send_notification(notification)?;

// After
cx.send_notification(notification)?;
```

### 6. Replace successor extension methods

The `sacp-proxy` crate is deleted. Replace `_from_successor` and `to_successor` extension methods with endpoint-targeted sends:

```rust
// Before (sacp-proxy)
cx.send_request(request.to_successor())?;
let inner = response.from_successor()?;

// After
cx.send_request_to(sacp::Agent, request)?;
```

### 7. Replace message.connection_cx() calls

`MessageAndCx` no longer has a `connection_cx()` method. Use the `cx` parameter passed to your handler:

```rust
// Before
.on_receive_message(async move |message: MessageAndCx<...>| {
    let cx = message.connection_cx();
    cx.send_notification(notif)?;
})

// After
.on_receive_message(async move |message: MessageCx, cx: JrConnectionCx<AgentToClient>| {
    cx.send_notification(notif)?;
})
```

### 8. Update Cargo.toml

Remove `sacp-proxy` dependency entirely - it's merged into `sacp`. MCP server types are now in `sacp::mcp_server`.

## Compiler error quick fixes

| Error | Fix |
|-------|-----|
| `expected 3 parameters, found 2` | Add `_cx` as third handler parameter |
| `trait JrPeer is not implemented` | Add role parameter: `JrConnectionCx<YourRole>` |
| `cannot find type MessageAndCx` | Use `MessageCx` |
| `method connection_cx not found` | Use `cx` parameter passed to handler |
| `cannot find to_successor/from_successor` | Use `send_request_to(sacp::Agent, ...)` |
| `cannot find sacp_proxy` | Remove dep, use `sacp::mcp_server` |
