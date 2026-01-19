# Building an Agent

This chapter explains how to build an ACP agent using the `sacp` crate.

## Overview

An agent is the component that provides AI model behavior in an ACP system. It receives prompts from clients (editors like Zed or Claude Code) and returns responses. Agents can also work as the final component in a conductor proxy chain.

## Quick Start

Here's a minimal agent that handles initialization:

```rust
use sacp::AgentToClient;
use sacp::schema::{AgentCapabilities, InitializeRequest, InitializeResponse};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::main]
async fn main() -> Result<(), sacp::Error> {
    Agent::builder()
        .name("my-agent")
        .on_receive_request(
            async move |req: InitializeRequest, request_cx, _cx| {
                request_cx.respond(InitializeResponse {
                    protocol_version: req.protocol_version,
                    agent_capabilities: AgentCapabilities::default(),
                    auth_methods: Default::default(),
                    agent_info: Default::default(),
                    meta: Default::default(),
                })
            },
            sacp::on_receive_request!(),
        )
        .serve(sacp::ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .await
}
```

## Core Concepts

### The Builder Pattern

Agents are built using `Agent::builder()`. You register handlers for different message types, then call `.serve()` with a transport:

```rust
Agent::builder()
    .name("my-agent")                           // For logging/debugging
    .on_receive_request(handler1, macro1)       // Handle specific request type
    .on_receive_request(handler2, macro2)       // Chain multiple handlers
    .on_receive_notification(handler3, macro3)  // Handle notifications
    .serve(transport)
    .await
```

### Request Handlers

Request handlers receive three parameters:

1. **The request** - Typed by what you're handling (e.g., `InitializeRequest`, `PromptRequest`)
2. **The request context** (`JrRequestCx`) - Used to send the response
3. **The connection context** (`JrConnectionCx`) - Used for sending notifications or spawning tasks

```rust
.on_receive_request(
    async move |request: PromptRequest, request_cx, cx| {
        // Process the request...
        
        // Send the response
        request_cx.respond(PromptResponse {
            stop_reason: StopReason::EndTurn,
            meta: None,
        })
    },
    sacp::on_receive_request!(),
)
```

### The Witness Macro

Every handler registration requires a "witness" macro as the final parameter. This is a workaround for missing Rust language features (return-type notation). Always use the matching macro:

- `.on_receive_request(..., sacp::on_receive_request!())`
- `.on_receive_notification(..., sacp::on_receive_notification!())`
- `.on_receive_message(..., sacp::on_receive_message!())`

### Sending Notifications

Use the connection context to send notifications back to the client:

```rust
.on_receive_request(
    async move |request: PromptRequest, request_cx, cx| {
        // Send streaming content
        cx.send_notification(SessionNotification {
            session_id: request.session_id.clone(),
            update: SessionUpdate::AgentMessageChunk(ContentChunk {
                content: "Hello!".into(),
                meta: None,
            }),
            meta: None,
        })?;

        // Complete the request
        request_cx.respond(PromptResponse {
            stop_reason: StopReason::EndTurn,
            meta: None,
        })
    },
    sacp::on_receive_request!(),
)
```

### Spawning Background Work

Don't block the message loop with long-running operations. Use `cx.spawn()` to run work in the background:

```rust
.on_receive_request(
    async move |request: PromptRequest, request_cx, cx| {
        let cx_clone = cx.clone();
        cx.spawn(async move {
            // Long-running AI inference...
            let response = generate_response(&request).await;
            
            // Send notification with result
            cx_clone.send_notification(SessionNotification { /* ... */ })?;
            
            // Complete the request
            request_cx.respond(PromptResponse {
                stop_reason: StopReason::EndTurn,
                meta: None,
            })
        })
    },
    sacp::on_receive_request!(),
)
```

## Building a Reusable Agent Component

For agents that can be composed into larger systems (e.g., with the conductor), implement the `Component` trait:

```rust
use sacp::{AgentToClient, Component};
use sacp::schema::*;

pub struct MyAgent {
    config: AgentConfig,
}

impl Component for MyAgent {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        Agent::builder()
            .name("my-agent")
            .on_receive_request(
                async |req: InitializeRequest, request_cx, _cx| {
                    request_cx.respond(InitializeResponse {
                        protocol_version: req.protocol_version,
                        agent_capabilities: AgentCapabilities::default(),
                        auth_methods: Default::default(),
                        agent_info: Default::default(),
                        meta: Default::default(),
                    })
                },
                sacp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |req: PromptRequest, request_cx, cx| {
                        agent.handle_prompt(req, request_cx, cx).await
                    }
                },
                sacp::on_receive_request!(),
            )
            .connect_to(client)?
            .serve()
            .await
    }
}
```

Note the difference: `.serve(transport)` for standalone agents vs `.connect_to(client)?.serve()` for composable components.

## Handling Multiple Request Types

Chain multiple `.on_receive_request()` calls to handle different message types. Handlers are tried in order until one matches:

```rust
Agent::builder()
    .name("my-agent")
    .on_receive_request(
        async |req: InitializeRequest, request_cx, _cx| {
            request_cx.respond(InitializeResponse { /* ... */ })
        },
        sacp::on_receive_request!(),
    )
    .on_receive_request(
        async |req: NewSessionRequest, request_cx, _cx| {
            request_cx.respond(NewSessionResponse { /* ... */ })
        },
        sacp::on_receive_request!(),
    )
    .on_receive_request(
        async |req: PromptRequest, request_cx, cx| {
            // Handle prompts...
            request_cx.respond(PromptResponse { /* ... */ })
        },
        sacp::on_receive_request!(),
    )
    .serve(transport)
    .await
```

## Catch-All Handler

Use `.on_receive_message()` to handle any message not caught by specific handlers:

```rust
Agent::builder()
    .name("my-agent")
    .on_receive_request(/* ... specific handlers ... */)
    .on_receive_message(
        async move |message: MessageCx, cx| {
            // Return an error for unhandled messages
            message.respond_with_error(
                sacp::util::internal_error("Unhandled message type"),
                cx,
            )
        },
        sacp::on_receive_message!(),
    )
    .serve(transport)
    .await
```

## Protocol Types

The `sacp::schema` module provides all ACP protocol types:

```rust
use sacp::schema::{
    // Initialization
    InitializeRequest, InitializeResponse,
    AgentCapabilities,
    
    // Sessions
    NewSessionRequest, NewSessionResponse,
    LoadSessionRequest, LoadSessionResponse,
    SessionId,
    
    // Prompts
    PromptRequest, PromptResponse,
    ContentBlock, ContentChunk,
    StopReason,
    
    // Notifications
    SessionNotification, SessionUpdate,
    
    // MCP
    McpServer,
};
```

## Complete Example

See the [elizacp](https://github.com/symposium-dev/symposium-acp/tree/main/src/elizacp) crate for a complete working agent implementation with session management and MCP tool support.

## Next Steps

- See [Protocol Reference](./protocol.md) for message format details
- Read the `sacp` crate rustdoc for full API documentation
- Check the [ACP specification](https://agentclientprotocol.com/) for protocol details
