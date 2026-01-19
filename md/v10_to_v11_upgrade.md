# Upgrading from sacp v10 to v11

This guide covers the breaking changes in sacp v11 and provides patterns for upgrading your code.

## Overview

v11 introduces a simpler Role-based API that replaces the previous Link/Peer system. The main goals were:
- Eliminate the `Jr` prefix from type names
- Replace Link types (which encode both sides of a connection) with Role types (which encode one side)
- Simplify the builder/connection API with clearer method names
- Unify peer types with role types

## Quick Reference

### Type Renames

| v10 | v11 |
|-----|-----|
| `Component<L>` | `ConnectTo<R>` |
| `DynComponent<L>` | `DynConnectTo<R>` |
| `ClientToAgent` | `Client` |
| `AgentToClient` | `Agent` |
| `ProxyToConductor` | `Proxy` |
| `ConductorToProxy` | `Conductor` |
| `AgentPeer` | `Agent` |
| `ClientPeer` | `Client` |
| `ConductorPeer` | `Conductor` |
| `JrConnectionCx<L>` | `ConnectionTo<R>` |
| `JrRequestCx<T>` | `Responder<T>` |
| `JrResponseCx<T>` | `ResponseRouter<T>` |
| `JrResponse<T>` | `SentRequest<T>` |
| `JrResponsePayload` | `JsonRpcResponse` |
| `JrRequest` | `JsonRpcRequest` |
| `JrNotification` | `JsonRpcNotification` |
| `JrMessage` | `JsonRpcMessage` |
| `JrMessageHandler` | `HandleDispatchFrom` |
| `handle_message` (trait method) | `handle_dispatch_from` |
| `JrResponder` | `RunWithConnectionTo` (or just `Run` alias) |
| `MessageCx` | `Dispatch` |
| `MatchMessage` | `MatchDispatch` |
| `MatchMessageFrom` | `MatchDispatchFrom` |
| `Conductor` (struct) | `ConductorImpl` |

### Method Renames

| v10 | v11 |
|-----|-----|
| `ClientToAgent::builder()` | `Client.builder()` |
| `AgentToClient::builder()` | `Agent.builder()` |
| `ProxyToConductor::builder()` | `Proxy.builder()` |
| `.serve(transport)` | `.connect_to(transport)` |
| `.run_until(transport, closure)` | `.connect_with(transport, closure)` |
| `.connect_to(transport)?.serve()` | `.connect_to(transport)` |
| `.into_server()` | `.into_channel_and_future()` |
| `on_receive_message(...)` | `on_receive_dispatch(...)` |
| `on_receive_message!()` | `on_receive_dispatch!()` |
| `.forward_to_request_cx(cx)` | `.forward_response_to(responder)` |

### Callback Parameter Renames

In request handlers, the context parameters have been renamed:

| v10 | v11 |
|-----|-----|
| `request_cx` | `responder` |
| `response_cx` | `router` |
| `cx` (generic connection) | `connection` |
| `agent_cx` | `connection_to_agent` |
| `client_cx` | `connection_to_client` |
| `editor_cx` | `connection_to_editor` |
| `conductor_cx` | `connection_to_conductor` |

---

## Common Upgrade Patterns

### Pattern 1: Client connecting to an agent

**v10:**
```rust
use sacp::{ClientToAgent, Component};

ClientToAgent::builder()
    .name("my-client")
    .run_until(transport, async |cx| {
        let response = cx.send_request(MyRequest {}).block_task().await?;
        Ok(())
    })
    .await
```

**v11:**
```rust
use sacp::{Client, ConnectTo};

Client.builder()
    .name("my-client")
    .connect_with(transport, async |connection| {
        let response = connection.send_request(MyRequest {}).block_task().await?;
        Ok(())
    })
    .await
```

### Pattern 2: Building an agent (reactive handler)

**v10:**
```rust
use sacp::{AgentToClient, Component};

AgentToClient::builder()
    .name("my-agent")
    .on_receive_request(async |req: InitializeRequest, request_cx, _cx| {
        request_cx.respond(InitializeResponse::new(req.protocol_version))
    }, sacp::on_receive_request!())
    .serve(transport)
    .await
```

**v11:**
```rust
use sacp::{Agent, ConnectTo};

Agent.builder()
    .name("my-agent")
    .on_receive_request(async |req: InitializeRequest, responder, _connection| {
        responder.respond(InitializeResponse::new(req.protocol_version))
    }, sacp::on_receive_request!())
    .connect_to(transport)
    .await
```

### Pattern 3: Implementing Component trait (now ConnectTo)

**v10:**
```rust
use sacp::{Component, AgentToClient};
use sacp::link::ClientToAgent;

impl Component<AgentToClient> for MyAgent {
    async fn serve(self, client: impl Component<ClientToAgent>) -> Result<(), sacp::Error> {
        AgentToClient::builder()
            .name("my-agent")
            // handlers...
            .serve(client)
            .await
    }
}
```

**v11:**
```rust
use sacp::{ConnectTo, Agent, Client};

impl ConnectTo<Client> for MyAgent {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> Result<(), sacp::Error> {
        Agent.builder()
            .name("my-agent")
            // handlers...
            .connect_to(client)
            .await
    }
}
```

### Pattern 4: Building a proxy

**v10:**
```rust
use sacp::{ProxyToConductor, ClientPeer, AgentPeer, Component};
use sacp::link::ConductorToProxy;

impl Component<ProxyToConductor> for MyProxy {
    async fn serve(self, client: impl Component<ConductorToProxy>) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("my-proxy")
            .on_receive_request_from(ClientPeer, async |req: MyRequest, request_cx, cx| {
                cx.send_request_to(AgentPeer, req)
                    .forward_to_request_cx(request_cx)
            }, sacp::on_receive_request!())
            .serve(client)
            .await
    }
}
```

**v11:**
```rust
use sacp::{Proxy, Client, Agent, Conductor, ConnectTo};

impl ConnectTo<Conductor> for MyProxy {
    async fn connect_to(self, client: impl ConnectTo<Proxy>) -> Result<(), sacp::Error> {
        Proxy.builder()
            .name("my-proxy")
            .on_receive_request_from(Client, async |req: MyRequest, responder, cx| {
                cx.send_request_to(Agent, req)
                    .forward_response_to(responder)
            }, sacp::on_receive_request!())
            .connect_to(client)
            .await
    }
}
```

### Pattern 5: Creating dynamic component collections

**v10:**
```rust
use sacp::{DynComponent, Component};
use sacp::link::{AgentToClient, ProxyToConductor};

let proxies: Vec<DynComponent<ProxyToConductor>> = vec![
    DynComponent::new(Proxy1),
    DynComponent::new(Proxy2),
];

let agent: DynComponent<AgentToClient> = DynComponent::new(MyAgent);
```

**v11:**
```rust
use sacp::{DynConnectTo, ConnectTo, Client, Conductor};

let proxies: Vec<DynConnectTo<Conductor>> = vec![
    DynConnectTo::new(Proxy1),
    DynConnectTo::new(Proxy2),
];

let agent: DynConnectTo<Client> = DynConnectTo::new(MyAgent);
```

### Pattern 6: Response handling

**v10:**
```rust
use sacp::JrResponse;

async fn recv<T: sacp::JrResponsePayload + Send>(
    response: sacp::JrResponse<T>,
) -> Result<T, sacp::Error> {
    // ...
}
```

**v11:**
```rust
use sacp::SentRequest;

async fn recv<T: sacp::JsonRpcResponse + Send>(
    response: sacp::SentRequest<T>,
) -> Result<T, sacp::Error> {
    // ...
}
```

### Pattern 7: Custom message handlers

**v10:**
```rust
use sacp::{JrMessageHandler, MessageCx, Handled, JrConnectionCx};
use sacp::util::MatchMessage;

impl JrMessageHandler for MyHandler {
    type Link = sacp::link::UntypedLink;

    async fn handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Self::Link>,
    ) -> Result<Handled<MessageCx>, sacp::Error> {
        MatchMessage::new(message)
            .if_request(async |req: MyRequest, request_cx| {
                request_cx.respond(MyResponse {})
            })
            .done()
    }
}
```

**v11:**
```rust
use sacp::{HandleDispatchFrom, Dispatch, Handled, ConnectionTo};
use sacp::util::MatchDispatch;

impl HandleDispatchFrom for MyHandler {
    type Role = sacp::UntypedRole;

    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        cx: ConnectionTo<Self::Role>,
    ) -> Result<Handled<Dispatch>, sacp::Error> {
        MatchDispatch::new(message)
            .if_request(async |req: MyRequest, responder| {
                responder.respond(MyResponse {})
            })
            .done()
    }
}
```

### Pattern 8: on_receive_message → on_receive_dispatch

**v10:**
```rust
.on_receive_message(async |message: MessageCx, cx| {
    message.respond_with_error(sacp::Error::method_not_found(), cx)
}, sacp::on_receive_message!())
```

**v11:**
```rust
.on_receive_dispatch(async |message: Dispatch, cx| {
    message.respond_with_error(sacp::Error::method_not_found(), cx)
}, sacp::on_receive_dispatch!())
```

### Pattern 9: Using the conductor

**v10:**
```rust
use sacp_conductor::{Conductor, ProxiesAndAgent};

Conductor::new_agent("conductor", ProxiesAndAgent::new(agent), Default::default())
    .run(transport)
    .await
```

**v11:**
```rust
use sacp_conductor::{ConductorImpl, ProxiesAndAgent};

ConductorImpl::new_agent("conductor", ProxiesAndAgent::new(agent), Default::default())
    .run(transport)
    .await
```

### Pattern 10: MCP server type parameters

**v10:**
```rust
use sacp::mcp_server::McpServer;

// For proxies
let server = McpServer::<ProxyToConductor, _>::builder("tools").build();

// For sessions (client-side)
let server = McpServer::<ClientToAgent, _>::builder("tools").build();
```

**v11:**
```rust
use sacp::mcp_server::McpServer;

// For proxies
let server = McpServer::<Conductor, _>::builder("tools").build();

// For sessions (client-side)
let server = McpServer::<Agent, _>::builder("tools").build();
```

---

## Import Changes

**v10:**
```rust
use sacp::{
    Component, DynComponent,
    ClientToAgent, AgentToClient, ProxyToConductor,
    AgentPeer, ClientPeer, ConductorPeer,
    JrConnectionCx, JrRequestCx, JrResponse, JrResponsePayload,
    JrMessageHandler, MessageCx,
};
use sacp::link::{JrLink, ConductorToProxy, UntypedLink};
```

**v11:**
```rust
use sacp::{
    ConnectTo, DynConnectTo,
    Client, Agent, Proxy, Conductor,
    ConnectionTo, Responder, SentRequest, JsonRpcResponse,
    HandleDispatchFrom, Dispatch,
    UntypedRole,
};
```

---

## Conceptual Changes

### Role vs Link

In v10, `Link` types encoded both sides of a connection:
- `ClientToAgent` = "I am a client talking to an agent"
- `AgentToClient` = "I am an agent talking to a client"

In v11, `Role` types encode the counterpart you connect to:
- `impl ConnectTo<Client>` = "I can connect to a client" (i.e., I am an agent)
- `impl ConnectTo<Agent>` = "I can connect to an agent" (i.e., I am a client)

### Unified Peer/Role Types

In v10, peers were separate types (`AgentPeer`, `ClientPeer`) from links.

In v11, `Agent`, `Client`, `Proxy`, and `Conductor` are both:
- Role types (used as type parameters)
- Peer selectors (used in `send_request_to(Agent, ...)`)
- Builder starters (`Agent.builder()`)

### Builder Method Naming

The new method names better describe what they do:
- `builder()` - "Start building a connection from this role"
- `connect_to(transport)` - "Connect to this transport (reactive mode)"
- `connect_with(transport, closure)` - "Connect with this transport and run closure (active mode)"

---

## Migration Tips

1. **Start with imports**: Update your `use` statements first to get the new types in scope.

2. **Search and replace**: Many renames are mechanical:
   - `ClientToAgent::builder()` → `Client.builder()`
   - `AgentToClient::builder()` → `Agent.builder()`
   - `ProxyToConductor::builder()` → `Proxy.builder()`
   - `.serve(` → `.connect_to(`
   - `.run_until(` → `.connect_with(`
   - `request_cx` → `responder`
   - `MessageCx` → `Dispatch`

3. **Component trait**: Change `impl Component<AgentToClient>` to `impl ConnectTo<Client>` - remember the role is your counterpart, not yourself.

4. **Peer types in handlers**: Change `ClientPeer` to `Client`, `AgentPeer` to `Agent`, etc.

5. **The conductor**: Rename `Conductor::` to `ConductorImpl::` in conductor creation calls.

6. **Test helpers**: Update `JrResponse<T>` to `SentRequest<T>` and `JrResponsePayload` to `JsonRpcResponse`.
