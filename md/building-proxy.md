# Building a Proxy

This chapter explains how to build a proxy component using the `sacp` crate.

## Overview

A proxy sits between clients and agents, intercepting and transforming messages. Proxies can:

- Modify prompts before they reach the agent
- Transform responses before they reach the client
- Provide additional MCP tools
- Add logging, metrics, or other side effects

Proxies are designed to work with the **conductor**, which chains multiple proxies together and routes messages between them.

## Quick Start

Here's a minimal proxy that adds a ">" prefix to all agent messages:

```rust
use sacp::{Agent, Client, Component, ProxyToConductor};
use sacp::schema::{ContentBlock, ContentChunk, SessionNotification, SessionUpdate};

pub async fn run_arrow_proxy(transport: impl Component) -> Result<(), sacp::Error> {
    ProxyToConductor::builder()
        .name("arrow-proxy")
        .on_receive_notification_from(
            Agent,
            async |mut notification: SessionNotification, cx| {
                // Modify content from the agent
                if let SessionUpdate::AgentMessageChunk(ContentChunk { content, .. }) = 
                    &mut notification.update 
                {
                    if let ContentBlock::Text(text) = content {
                        text.text = format!(">{}", text.text);
                    }
                }

                // Forward to client
                cx.send_notification_to(Client, notification)?;
                Ok(())
            },
            sacp::on_receive_notification!(),
        )
        .connect_to(transport)?
        .serve()
        .await
}
```

## Core Concepts

### Endpoints: Agent and Client

Proxies communicate with two endpoints:

- **`Agent`** - The downstream direction (toward the AI agent)
- **`Client`** - The upstream direction (toward the editor)

When you receive a message, you typically forward it (possibly modified) to the other endpoint:

```rust
use sacp::{Agent, Client};

// Message from client -> forward to agent
cx.send_request_to(Agent, modified_request)?;

// Message from agent -> forward to client  
cx.send_notification_to(Client, modified_notification)?;
```

### The Builder Pattern

Proxies are built using `ProxyToConductor::builder()`:

```rust
ProxyToConductor::builder()
    .name("my-proxy")
    // Handle messages from client (default)
    .on_receive_request(handler, sacp::on_receive_request!())
    // Handle messages from agent
    .on_receive_notification_from(Agent, handler, sacp::on_receive_notification!())
    .connect_to(transport)?
    .serve()
    .await
```

### Message Direction

By default, `.on_receive_request()` and `.on_receive_notification()` handle messages from the **client** direction.

To handle messages from the **agent**, use the `_from` variants:

```rust
// From client (default)
.on_receive_request(handler, macro!())
.on_receive_notification(handler, macro!())

// From agent
.on_receive_request_from(Agent, handler, macro!())
.on_receive_notification_from(Agent, handler, macro!())
```

### The Witness Macro

Like agents, every handler registration requires a witness macro:

- `.on_receive_request(..., sacp::on_receive_request!())`
- `.on_receive_notification(..., sacp::on_receive_notification!())`
- `.on_receive_message(..., sacp::on_receive_message!())`

## Common Patterns

### Pass-Through Proxy

A proxy that doesn't register any handlers passes all messages through unchanged:

```rust
pub async fn run_passthrough(transport: impl Component) -> Result<(), sacp::Error> {
    ProxyToConductor::builder()
        .name("passthrough")
        .connect_to(transport)?
        .serve()
        .await
}
```

### Prompt Transformation

Modify prompts before they reach the agent:

```rust
use sacp::schema::{ContentBlock, PromptRequest, TextContent};

ProxyToConductor::builder()
    .name("context-injector")
    .on_receive_request(
        async |mut request: PromptRequest, _request_cx, cx| {
            // Prepend context to the prompt
            let context = ContentBlock::Text(TextContent {
                text: "You are a helpful assistant.".to_string(),
                meta: None,
            });
            request.prompt.insert(0, context);

            // Forward modified request to agent
            cx.send_request_to(Agent, request)?;
            Ok(())
        },
        sacp::on_receive_request!(),
    )
    .connect_to(transport)?
    .serve()
    .await
```

### Response Filtering

Modify or filter responses from the agent:

```rust
ProxyToConductor::builder()
    .name("filter-proxy")
    .on_receive_notification_from(
        Agent,
        async |notification: SessionNotification, cx| {
            // Filter or modify the notification
            if should_forward(&notification) {
                cx.send_notification_to(Client, notification)?;
            }
            Ok(())
        },
        sacp::on_receive_notification!(),
    )
    .connect_to(transport)?
    .serve()
    .await
```

### Providing MCP Tools

Proxies can provide MCP tools that agents can use:

```rust
use sacp::mcp_server::McpServer;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoParams {
    message: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EchoOutput {
    result: String,
}

pub async fn run_mcp_proxy(transport: impl Component) -> Result<(), sacp::Error> {
    let mcp_server = McpServer::builder("my-tools")
        .instructions("Helpful tools for the agent")
        .tool_fn_mut(
            "echo",
            "Echoes the input message",
            async |params: EchoParams, _context| {
                Ok(EchoOutput {
                    result: format!("Echo: {}", params.message),
                })
            },
            sacp::tool_fn_mut!(),
        )
        .build();

    ProxyToConductor::builder()
        .name("mcp-proxy")
        .with_mcp_server(mcp_server)
        .connect_to(transport)?
        .serve()
        .await
}
```

The `tool_fn_mut!()` macro is required due to Rust language limitations (similar to the handler witness macros).

## Building a Reusable Proxy Component

For proxies that can be composed into the conductor, implement `Component`:

```rust
use sacp::{Agent, Client, Component, ProxyToConductor};

pub struct MyProxy {
    config: ProxyConfig,
}

impl Component for MyProxy {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("my-proxy")
            .on_receive_notification_from(
                Agent,
                {
                    let config = self.config.clone();
                    async move |notification: SessionNotification, cx| {
                        // Use config here...
                        cx.send_notification_to(Client, notification)?;
                        Ok(())
                    }
                },
                sacp::on_receive_notification!(),
            )
            .connect_to(client)?
            .serve()
            .await
    }
}
```

Then use it with the conductor:

```rust
use sacp_conductor::Conductor;

Conductor::new("my-conductor")
    .proxy(MyProxy::new(config))
    .agent(my_agent)
    .serve(transport)
    .await
```

## Handler Parameters

### Request Handlers

Request handlers receive three parameters:

```rust
.on_receive_request(
    async |request: SomeRequest, request_cx, cx| {
        // request: The typed request
        // request_cx: JrRequestCx - for responding to this specific request
        // cx: JrConnectionCx - for sending other messages, spawning tasks
        
        // Forward to agent (response routing is automatic)
        cx.send_request_to(Agent, request)?;
        Ok(())
    },
    sacp::on_receive_request!(),
)
```

### Notification Handlers

Notification handlers receive two parameters:

```rust
.on_receive_notification_from(
    Agent,
    async |notification: SomeNotification, cx| {
        // notification: The typed notification
        // cx: JrConnectionCx - for sending messages
        
        cx.send_notification_to(Client, notification)?;
        Ok(())
    },
    sacp::on_receive_notification!(),
)
```

## Complete Examples

- **[arrow_proxy.rs](https://github.com/symposium-dev/symposium-acp/blob/main/src/sacp-test/src/arrow_proxy.rs)** - Simple message transformation
- **[proxy.rs](https://github.com/symposium-dev/symposium-acp/blob/main/src/sacp-conductor/tests/mcp_integration/proxy.rs)** - MCP tool provider

## Next Steps

- See [Conductor Implementation](./conductor.md) for how proxies are composed
- See [MCP Bridge](./mcp-bridge.md) for MCP-over-ACP details
- Read the `sacp` crate rustdoc for full API documentation
