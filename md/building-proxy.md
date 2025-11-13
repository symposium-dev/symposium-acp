# Building a Proxy

This chapter explains how to build a proxy component using the `sacp-proxy` crate.

## Overview

A proxy component intercepts messages between editors and agents, transforming them or adding side effects. Proxies are built using the `sacp-proxy` framework.

## Basic Structure

```rust
use sacp_proxy::{AcpProxyExt, JsonRpcCxExt, ProxyHandler};
use sacp::{JsonRpcConnection, JsonRpcHandler};

// Your proxy's main handler
struct MyProxy {
    // State fields
}

impl JsonRpcHandler for MyProxy {
    async fn handle_message(&mut self, message: MessageAndCx) -> Result<Handled> {
        match message {
            // Handle messages from upstream (editor direction)
            MessageAndCx::Request(req, cx) => {
                match req {
                    // Transform and forward
                    AcpRequest::Prompt(mut prompt) => {
                        // Modify the prompt
                        prompt.messages.insert(0, my_context);
                        
                        // Forward to successor
                        cx.send_request_to_successor(prompt)
                          .await_when_result_received(|result| {
                              cx.respond_with_result(result)
                          })
                    }
                    // Other message types...
                }
            }
            MessageAndCx::Notification(notif, cx) => {
                // Handle notifications
            }
        }
    }
}
```

## Key Traits

### `AcpProxyExt`

Provides methods for handling messages from the successor:

```rust
use sacp_proxy::AcpProxyExt;

connection
    .on_receive_request_from_successor(|req, cx| async move {
        // Handle request from downstream component
    })
    .on_receive_notification_from_successor(|notif, cx| async move {
        // Handle notification from downstream
    })
    .proxy() // Enable automatic proxy capability handshake
```

### `JsonRpcCxExt`

Provides methods for sending to successor:

```rust
use sacp_proxy::JsonRpcCxExt;

// Send request and handle response
cx.send_request_to_successor(request)
  .await_when_result_received(|result| {
      cx.respond_with_result(result)
  })

// Send notification (fire and forget)
cx.send_notification_to_successor(notification)
```

## Proxy Patterns

### Pass-through Proxy

The simplest proxy forwards everything unchanged:

```rust
impl JsonRpcHandler for PassThrough {
    async fn handle_message(&mut self, message: MessageAndCx) -> Result<Handled> {
        match message {
            MessageAndCx::Request(req, cx) => {
                cx.send_request_to_successor(req)
                  .await_when_result_received(|r| cx.respond_with_result(r))
            }
            MessageAndCx::Notification(notif, cx) => {
                cx.send_notification_to_successor(notif)
            }
        }
    }
}
```

### Initialization Injection

Inject context or configuration during initialization:

```rust
MessageAndCx::Request(AcpRequest::Initialize(mut init), cx) => {
    // Add your capabilities
    init.capabilities.my_feature = true;
    
    cx.send_request_to_successor(init)
      .await_when_result_received(|result| {
          cx.respond_with_result(result)
      })
}
```

### Prompt Transformation

Modify prompts before they reach the agent:

```rust
MessageAndCx::Request(AcpRequest::Prompt(mut prompt), cx) => {
    // Prepend system context
    let context_message = Message {
        role: Role::User,
        content: vec![Content::Text { text: context }],
    };
    prompt.messages.insert(0, context_message);
    
    cx.send_request_to_successor(prompt)
      .await_when_result_received(|result| {
          cx.respond_with_result(result)
      })
}
```

### MCP Server Provider

Provide MCP servers to the agent:

```rust
use sacp_proxy::AcpProxyExt;

connection
    .provide_mcp(my_mcp_server_uuid, my_mcp_handler)
    .proxy()
```

See the Protocol Reference for details on the MCP-over-ACP protocol.

## Complete Example

For a complete example of a production proxy, see the [sparkle-acp-proxy](https://github.com/nikomatsakis/sparkle-acp-proxy) implementation.

## Next Steps

- See [Protocol Reference](./protocol.md) for message format details
- Read the `sacp-proxy` crate documentation for API details
- Study the sparkle-acp-proxy implementation for patterns
