# Trace Viewer

`sacp-trace-viewer` is a debugging tool that visualizes message flow between ACP components as an interactive sequence diagram.

## Overview

When debugging ACP proxy chains, understanding the flow of messages between components is critical. The trace viewer provides:

- **Visual sequence diagrams** showing messages flowing between Client, Proxies, and Agent
- **Request/response correlation** with arrows linking requests to their responses
- **Tracing log integration** showing component logs inline on their timeline
- **Session color-coding** to distinguish multiple ACP sessions
- **Zoom and filter** to navigate large traces

## Architecture

The system has two parts:

1. **Event capture** (in `sacp-conductor`) - appends structured JSON events to a `.jsons` file
2. **Viewer** (in `sacp-trace-viewer`) - serves an interactive HTML/JS visualization

### Workflow

```bash
# Run conductor with tracing enabled
sacp-conductor agent --trace ./trace.jsons "proxy1" "proxy2" "agent"

# View the trace (works during or after the run)
sacp-trace-viewer ./trace.jsons
# Opens browser to http://localhost:PORT
```

## Event Schema

Events are stored as newline-delimited JSON (`.jsons` file). Each line is a self-contained event.

### Common Fields

All events share these fields:

```typescript
interface BaseEvent {
  // Monotonic timestamp (seconds since trace start, f64)
  ts: number;
  
  // Event type discriminator
  type: "request" | "response" | "notification" | "trace";
}
```

### Request Event

A JSON-RPC request from one component to another.

```typescript
interface RequestEvent extends BaseEvent {
  type: "request";
  
  // Protocol: "acp" for ACP messages, "mcp" for MCP-over-ACP messages
  protocol: "acp" | "mcp";
  
  // Source component (e.g., "client", "proxy:0", "proxy:1", "agent")
  from: string;
  
  // Destination component
  to: string;
  
  // JSON-RPC request ID (for correlating with response)
  id: number | string;
  
  // JSON-RPC method name
  method: string;
  
  // ACP session ID, if known (null before session/new completes)
  session: string | null;
  
  // Full request params (may be large)
  params: object;
}
```

### Response Event

A JSON-RPC response to a prior request.

```typescript
interface ResponseEvent extends BaseEvent {
  type: "response";
  
  // Source component (who sent the response)
  from: string;
  
  // Destination component (who receives the response)
  to: string;
  
  // JSON-RPC request ID this responds to
  id: number | string;
  
  // True if this is an error response
  is_error: boolean;
  
  // Response result or error object
  payload: object;
}
```

### Notification Event

A JSON-RPC notification (no response expected).

```typescript
interface NotificationEvent extends BaseEvent {
  type: "notification";
  
  // Protocol: "acp" for ACP messages, "mcp" for MCP-over-ACP messages
  protocol: "acp" | "mcp";
  
  from: string;
  to: string;
  method: string;
  session: string | null;
  params: object;
}
```

### Trace Event

A tracing log line from a component (captured from stderr or tracing subscriber).

```typescript
interface TraceEvent extends BaseEvent {
  type: "trace";
  
  // Which component emitted this log
  component: string;
  
  // Log level
  level: "trace" | "debug" | "info" | "warn" | "error";
  
  // Log message
  message: string;
  
  // Optional structured fields from tracing spans
  fields?: Record<string, unknown>;
}
```

## Component Naming

Components are identified by strings:

- `"client"` - the upstream client (editor/IDE)
- `"proxy:0"`, `"proxy:1"`, etc. - proxy components by index
- `"agent"` - the final agent component

The viewer displays these as swimlane columns in the sequence diagram.

## Viewer Features

### Sequence Diagram

The main view is a vertical timeline with component columns (swimlanes):

```
  Client         Proxy:0        Proxy:1         Agent
    │               │              │               │
    │──initialize──>│              │               │
    │               │──initialize──>│              │
    │               │              │──initialize──>│
    │               │              │<╌╌╌╌╌╌╌╌╌╌╌╌╌╌│
    │               │<╌╌╌╌╌╌╌╌╌╌╌╌╌│              │
    │<╌╌╌╌╌╌╌╌╌╌╌╌╌╌│              │               │
    │               │              │               │
    │──session/new─>│              │               │
    │               │──session/new─>│              │
    │               │              │──session/new─>│
    │               │              │<╌╌╌╌╌╌╌╌╌╌╌╌╌╌│
    │               │<╌╌╌╌╌╌╌╌╌╌╌╌╌│              │
    │<╌╌╌╌╌╌╌╌╌╌╌╌╌╌│              │               │
    │               │              │               │
    │               │<─tools/call──│              │  (mcp)
    │               │╌╌╌╌╌╌╌╌╌╌╌╌╌>│              │
    │               │              │               │
```

- **Requests/notifications** - solid arrows with method name as inline label
- **Responses** - dashed arrows pointing back, linked to their originating request
  - No label (click to see payload)
  - Visual indicator for error responses (e.g., red color)
- **MCP messages** - distinguished visually (different color or line style), flow from agent back to proxies
- **Trace logs** - inline markers on a component's column
- **Click any arrow** - expands full JSON payload in a detail panel

### Interactions

- **Zoom** - mouse wheel or pinch to zoom in/out on the timeline
- **Pan** - drag to navigate
- **Click message** - expands full JSON payload in sidebar panel
- **Filter by session** - checkbox to show/hide sessions by color
- **Filter by type** - show/hide requests, notifications, traces
- **Search** - find messages by method name or content

### Color Coding

- Each ACP session gets a distinct color
- Requests and their responses share the same color
- Error responses highlighted in red
- Trace log levels have distinct colors (debug=gray, info=blue, warn=yellow, error=red)

## Implementation Notes

### Idealized View

The trace shows the *logical* message flow between components, hiding conductor implementation details. The conductor transforms internal messages before logging:

| Internal Message | Logged As |
|-----------------|-----------|
| `_proxy/successor/foo` from proxy N | `foo` from proxy N to proxy N+1 (protocol: acp) |
| `_mcp/request` with inner method `bar` | `bar` from agent to proxy (protocol: mcp) |
| `_mcp/notify` with inner method `baz` | `baz` from agent to proxy (protocol: mcp) |

This means the viewer shows what conceptually happened without exposing the routing machinery.

### sacp-conductor changes

**CLI usage:**
```bash
sacp-conductor agent --trace ./trace.jsons "proxy1" "proxy2" "agent"
```

**Programmatic usage:**
```rust
Conductor::new(name, components, mcp_bridge_mode)
    .trace_to("./trace.jsons")
    .into_handler_chain()
    // ...
```

**Implementation:**
- Capture happens in `handle_conductor_message` where all messages flow
- Conductor transforms messages to idealized form before logging
- Events appended as JSON lines (one `writeln!` per event)
- File opened in append mode, flushed after each event

### sacp-trace-viewer crate

- Single binary with embedded static assets (HTML/JS/CSS)
- Uses a lightweight HTTP server (e.g., `axum` or `tiny_http`)
- Endpoints:
  - `GET /` - serves the viewer HTML
  - `GET /events` - streams the `.jsons` file content
  - `GET /events?after=<offset>` - for tailing (returns events after byte offset)
- Viewer JS fetches events, renders SVG sequence diagram
- No build step for JS - vanilla JS is sufficient for this use case
