# Building a Proxy

This chapter explains how to build a proxy component using the `sacp` and `sacp-proxy` crates.

## Overview

A proxy component intercepts messages between editors and agents, transforming them or adding side effects. Proxies are built using the `Component` trait from `sacp` and helper extensions from `sacp-proxy`.

## Basic Structure

```rust
use sacp::{Component, JrHandlerChain, MessageAndCx};
use sacp::schema::{PromptRequest, InitializeRequest};

struct MyProxy {
    // State fields
}

impl Component for MyProxy {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        JrHandlerChain::new()
            .name("my-proxy")
            .on_receive_request(async move |req: PromptRequest, cx| {
                // Transform the prompt
                let mut modified = req;
                modified.messages.insert(0, my_context_message());
                
                // Forward to successor and relay response back
                cx.send_request(modified)
                    .await_when_result_received(async move |result| {
                        cx.respond_with_result(result)
                    })
            })
            .serve(client)
            .await
    }
}
```

## Key Concepts

### Component Composition

The `Component` trait enables composition through the `serve()` method. Each component receives the next component in the chain as the `client` parameter:

```rust
impl Component for MyProxy {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Set up handlers that forward to `client`
        JrHandlerChain::new()
            .on_receive_request(async move |req, cx| {
                // Process and forward to client
            })
            .serve(client)
            .await
    }
}
```

### Non-Blocking Message Handlers

**Important:** Message handlers run on the event loop. Blocking in a handler will prevent the connection from processing new messages.

Use `await_when_*` methods to avoid blocking:

```rust
.on_receive_request(async move |req: PromptRequest, cx| {
    // This sends the request without blocking
    cx.send_request(req)
        .await_when_result_received(async move |result| {
            // This runs after the response is received
            cx.respond_with_result(result)
        })
})
```

Or use `cx.spawn()` for background work:

```rust
.on_receive_request(async move |req: PromptRequest, cx| {
    let connection_cx = cx.connection_cx();
    cx.spawn(async move {
        // Expensive work here won't block the message loop
        let result = expensive_computation(req).await;
        connection_cx.send_notification(ComputationComplete { result });
    });
    cx.respond(AckResponse {})
})
```

## Common Proxy Patterns

### Pass-through Proxy

The simplest proxy forwards everything unchanged:

```rust
impl Component for PassThrough {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Just serve the client directly - no interception needed
        client.serve(sacp::ByteStreams::stdio()).await
    }
}
```

For selective forwarding with some message handling:

```rust
impl Component for SelectiveProxy {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        JrHandlerChain::new()
            .name("selective-proxy")
            .on_receive_request(async move |req: PromptRequest, cx| {
                // Handle prompts specially
                let modified = transform_prompt(req);
                cx.send_request(modified)
                    .await_when_result_received(async move |result| {
                        cx.respond_with_result(result)
                    })
            })
            // All other messages forward through automatically
            .serve(client)
            .await
    }
}
```

### Initialization Enhancement

Intercept initialization to inject capabilities or configuration:

```rust
.on_receive_request(async move |req: InitializeRequest, cx| {
    // Forward to successor to get their capabilities
    cx.send_request(req)
        .await_when_result_received(async move |mut result| {
            match &mut result {
                Ok(init_response) => {
                    // Add our capabilities
                    init_response.agent_capabilities.my_feature = true;
                }
                Err(_) => {
                    // Pass errors through unchanged
                }
            }
            cx.respond_with_result(result)
        })
})
```

### Prompt Transformation

Modify prompts before they reach the agent:

```rust
.on_receive_request(async move |req: PromptRequest, cx| {
    let mut modified = req;
    
    // Prepend system context
    let context_message = Message {
        role: Role::User,
        content: Content::text("Additional context here"),
    };
    modified.messages.insert(0, context_message);
    
    cx.send_request(modified)
        .await_when_result_received(async move |result| {
            cx.respond_with_result(result)
        })
})
```

### Response Transformation

Transform responses before they return to the editor:

```rust
.on_receive_request(async move |req: PromptRequest, cx| {
    cx.send_request(req)
        .await_when_result_received(async move |result| {
            let transformed = match result {
                Ok(mut response) => {
                    // Modify the response
                    response.content = post_process(response.content);
                    Ok(response)
                }
                Err(e) => Err(e),
            };
            cx.respond_with_result(transformed)
        })
})
```

### MCP Server Provider

Provide MCP servers to downstream components using the `sacp-proxy` crate:

```rust
use sacp_proxy::ProxyExt;

impl Component for McpProvider {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        JrHandlerChain::new()
            .name("mcp-provider")
            .provide_mcp(self.server_id, self.mcp_handler)
            .serve(client)
            .await
    }
}
```

See the Protocol Reference for details on the MCP-over-ACP protocol.

### Stateful Proxy with Custom Handler

For more complex state management, implement `JrMessageHandler`:

```rust
use sacp::{JrMessageHandler, Handled};
use sacp::util::MatchMessage;
use std::sync::{Arc, Mutex};

struct StatefulProxy {
    state: Arc<Mutex<ProxyState>>,
}

impl JrMessageHandler for StatefulProxy {
    async fn handle_message(&mut self, message: MessageAndCx)
        -> Result<Handled<MessageAndCx>, sacp::Error>
    {
        MatchMessage::new(message)
            .if_request(async |req: PromptRequest, cx| {
                let mut state = self.state.lock().unwrap();
                state.request_count += 1;
                
                // Transform based on state
                let modified = state.transform_request(req);
                
                cx.send_request(modified)
                    .await_when_result_received(async move |result| {
                        cx.respond_with_result(result)
                    })
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "StatefulProxy"
    }
}
```

## Request Context Capabilities

The [`JrRequestCx`] provided to handlers offers several capabilities:

- **Respond to the request:** `cx.respond(response)` or `cx.respond_with_result(result)`
- **Send requests downstream:** `cx.send_request(request)` 
- **Send notifications:** `cx.send_notification(notification)`
- **Spawn background tasks:** `cx.spawn(future)`
- **Get connection context:** `cx.connection_cx()` for contexts not tied to a specific request

## Complete Example

Here's a complete proxy that logs all prompts and adds timing information:

```rust
use sacp::{Component, JrHandlerChain};
use sacp::schema::{PromptRequest, PromptResponse};
use std::time::Instant;

struct LoggingProxy;

impl Component for LoggingProxy {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        JrHandlerChain::new()
            .name("logging-proxy")
            .on_receive_request(async move |req: PromptRequest, cx| {
                let start = Instant::now();
                tracing::info!(
                    "Prompt received with {} messages",
                    req.messages.len()
                );
                
                cx.send_request(req)
                    .await_when_result_received(async move |result| {
                        let elapsed = start.elapsed();
                        tracing::info!("Response received in {:?}", elapsed);
                        cx.respond_with_result(result)
                    })
            })
            .serve(client)
            .await
    }
}
```

## Testing Your Proxy

Use `sacp-test` helpers for testing components:

```rust
#[tokio::test]
async fn test_my_proxy() {
    let proxy = MyProxy::new();
    let mock_agent = MockAgent::new();
    
    // Test the proxy
    proxy.serve(mock_agent).await.unwrap();
}
```

## Next Steps

- See [Protocol Reference](./protocol.md) for message format details
- Read the `sacp` crate documentation for complete API details
- Study the [sparkle-acp-proxy](https://github.com/nikomatsakis/sparkle-acp-proxy) for a production example
- Check [Component Architecture](./pacp-components.md) for proxy chain design patterns
