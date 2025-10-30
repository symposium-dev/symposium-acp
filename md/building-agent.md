# Building an Agent

This chapter explains how to build an ACP agent using the `sacp` crate.

## Overview

An agent is the final component in a SACP proxy chain. It provides the base AI model behavior and doesn't need awareness of SACP - it's just a standard ACP agent.

However, the `sacp` crate provides useful types and utilities for building ACP agents.

## Core Types

The `sacp` crate provides Rust types for ACP protocol messages:

```rust
use sacp::{
    InitializeRequest, InitializeResponse,
    PromptRequest, PromptResponse,
    // ... other ACP types
};
```

These types handle:
- Serialization/deserialization
- Protocol validation
- Type safety for message handling

## JSON-RPC Foundation

The `sacp` crate includes a JSON-RPC layer that handles:

- Message framing over stdio or other transports
- Request/response correlation
- Notification handling
- Error propagation

```rust
use sacp::{JsonRpcConnection, JsonRpcHandler};

// Create a connection over stdio
let connection = JsonRpcConnection::new(stdin(), stdout(), my_handler);

// Run the message loop
connection.run().await?;
```

## Handler Pattern

Implement `JsonRpcHandler` to process ACP messages:

```rust
use sacp::{JsonRpcHandler, MessageAndCx, Handled};

struct MyAgent {
    // Agent state
}

impl JsonRpcHandler for MyAgent {
    async fn handle_message(&mut self, message: MessageAndCx) -> Result<Handled> {
        match message {
            MessageAndCx::Request(req, cx) => {
                match req {
                    AcpRequest::Initialize(init) => {
                        // Handle initialization
                        let response = InitializeResponse {
                            protocolVersion: "0.7.0",
                            serverInfo: ServerInfo { /* ... */ },
                            capabilities: Capabilities { /* ... */ },
                        };
                        cx.respond(response)?;
                        Ok(Handled::FullyHandled)
                    }
                    AcpRequest::Prompt(prompt) => {
                        // Call your AI model
                        let response = self.generate_response(prompt).await?;
                        cx.respond(response)?;
                        Ok(Handled::FullyHandled)
                    }
                    // ... other message types
                }
            }
            MessageAndCx::Notification(notif, cx) => {
                // Handle notifications
            }
        }
    }
}
```

## Working with Proxies

Your agent doesn't need to know about SACP proxies. However, there are some optional capabilities that improve proxy integration:

### MCP-over-ACP Support

If your agent can handle MCP servers declared with `acp:UUID` URLs, advertise the capability:

```rust
InitializeResponse {
    // ...
    _meta: json!({
        "mcp_acp_transport": true
    }),
}
```

This allows the conductor to skip bridging and pass MCP declarations through directly.

Without this capability, the conductor will automatically bridge MCP-over-ACP to stdio for you.

## Testing

The `sacp` crate provides test utilities:

```rust
#[cfg(test)]
mod tests {
    use sacp::testing::*;
    
    #[test]
    fn test_prompt_handling() {
        let agent = MyAgent::new();
        let response = agent.handle_prompt(test_prompt()).await?;
        assert_eq!(response.role, Role::Assistant);
    }
}
```

## Standard ACP Implementation

Remember: An agent built with `sacp` is a standard ACP agent. It will work:

- Directly with ACP editors (Zed, Claude Code, etc.)
- As the final component in a SACP proxy chain
- With any ACP-compatible tooling

The `sacp` crate just provides convenient Rust types and infrastructure.

## Next Steps

- See [Protocol Reference](./protocol.md) for message format details
- Read the `sacp` crate documentation for API details
- Check the [ACP specification](https://agentclientprotocol.com/) for protocol details
