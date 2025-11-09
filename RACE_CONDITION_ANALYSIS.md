# Race Condition Analysis: Notification vs Response Ordering

## Problem Summary

Integration tests for nested conductors are flaky (~60% failure rate). When they fail, the test receives an empty response string even though the notification was sent by the agent with the correct text.

## Root Cause

**Response messages overtake notification messages** when forwarded through conductor layers due to different routing paths:

- **Responses** use a fast path: `reply_actor` with oneshot channels (`unbounded_send` - not awaited)
- **Notifications** use the normal handler pipeline: sequential processing through `incoming_protocol_actor`

### Evidence from Logs

From `test-logs/run_1.log` (failed test with session `c4cebfae-7cdf-461e-aa77-d9e7bfedd12e`):

```
14:01:24.672522Z - Eliza sends session/update notification with text "Hello. How are you feeling today?"
14:01:24.672580Z - Notification received at outer-conductor from eliza
...
14:01:24.674011Z - Inner conductor sends _proxy/successor/notification with ">Hello..."
14:01:24.674141Z - yolo_prompt receives session/prompt RESPONSE (130 μs later)
14:01:24.674230Z - main_fn processes response: "prompt response received"
14:01:24.674263Z - main actor terminated (with_client closure completes)
14:01:24.674274Z - Returns collected_text="" (EMPTY!)
```

**The notification was never processed by yolo_prompt's handler** because the connection terminated when the response arrived first.

## Message Flow

### Normal Flow (When It Works)

```
Eliza → session/update notification → outer-conductor → inner-conductor → yolo_prompt handler
      ↓ (processed first)
      notification updates collected_text = ">>Hello..."
      
Eliza → session/prompt response → outer-conductor → inner-conductor → yolo_prompt reply_actor
      ↓ (processed second)
      response completes future, returns collected_text (has text!)
```

### Race Condition Flow (When It Fails)

```
Eliza → session/update notification → outer-conductor (slow path through handlers)
Eliza → session/prompt response → outer-conductor (fast path through reply_actor)
      ↓ response overtakes notification!
      
yolo_prompt receives response FIRST
      ↓
      main_fn completes, connection terminates
      ↓
      notification arrives but connection is closed
      ↓
      Returns collected_text="" (EMPTY!)
```

## Architecture Issue

The conductor has a central event loop (line 150 in conductor.rs) that processes `ConductorMessage` events sequentially:

```rust
while let Some(message) = conductor_rx.next().await {
    state.handle_conductor_message(&cx, message, &mut conductor_tx).await?;
}
```

This **should** preserve message ordering. However, when handling `AgentToClient` messages (line 360-363), the conductor calls `send_message_to_predecessor_of`, which calls `send_proxied_message`, which **bypasses the central queue** and sends messages directly to component connections:

```rust
// Line 475 and others - directly calls component.send_proxied_message()
client.send_proxied_message(message)
self.components[index - 1].send_proxied_message(wrapped)
```

This `send_proxied_message` method (jsonrpc.rs:1119) splits into two different paths:
- **Requests**: `send_request().forward_to_request_cx()` → schedules callback via `reply_actor` (fast oneshot channel path)
- **Notifications**: `send_notification()` → goes through normal handler pipeline (slower)

Even though the conductor's central loop processes messages in order, once they leave via `send_proxied_message`, they take different routing paths with different speeds, causing reordering.

## The Invariant We Need

**Messages sent in sequence by an agent must arrive in the same sequence at the client, even when forwarded through multiple conductor layers.**

Specifically: If an agent sends `notification1`, then `response2`, the client must process `notification1` before `response2` completes its future.

## Affected Tests

- `test_nested_conductor_with_external_arrow_proxies` - ~60% failure rate
- `test_conductor_with_two_external_arrow_proxies` - ~25% failure rate

The in-process mock tests (`test_nested_conductor_with_arrow_proxies`) do not exhibit the same failure rate, suggesting the race window is related to timing in external process communication.

## The Fix

The solution routes response forwarding through the conductor's central message queue instead of using `forward_to_request_cx`, which spawned independent tasks that bypassed ordering.

### Changes Made

**1. Added `ConductorMessage::ForwardResponse` variant** (conductor.rs:883-892):
```rust
ForwardResponse {
    request_cx: JrRequestCx<serde_json::Value>,
    result: Result<serde_json::Value, sacp::Error>,
}
```

**2. Added handler for `ForwardResponse`** (conductor.rs:456-460):
```rust
ConductorMessage::ForwardResponse { request_cx, result } => {
    request_cx.respond_with_result(result)
}
```

**3. Updated request forwarding logic** (conductor.rs:648-660 and 805-826):

Instead of:
```rust
self.components[target_component_index]
    .send_request(request)
    .forward_to_request_cx(request_cx)  // Bypasses conductor queue!
```

Now:
```rust
let mut conductor_tx_clone = conductor_tx.clone();
self.components[target_component_index]
    .send_request(request)
    .await_when_result_received(async move |result| {
        conductor_tx_clone
            .send(ConductorMessage::ForwardResponse { request_cx, result })
            .await
            .map_err(|e| sacp::util::internal_error(format!("Failed to send response: {}", e)))
    })
```

This ensures responses go through the conductor's central queue and maintain ordering with notifications.

### Test Results

**Before fix:**
- `test_nested_conductor_with_external_arrow_proxies`: ~60% failure rate (6/10 runs failed)
- `test_conductor_with_two_external_arrow_proxies`: ~25% failure rate

**After fix:**
- 30 consecutive runs of all conductor tests: **100% pass rate**
- No failures observed in any test

## Files Involved

- `src/sacp/src/jsonrpc/actors.rs` - Core actor implementation (reply_actor, incoming_protocol_actor)
- `src/sacp-conductor/src/conductor.rs` - Message forwarding logic
- `src/sacp-conductor/tests/nested_conductor.rs` - Flaky integration tests
- `src/sacp-test/src/test_client.rs` - Test helper with notification handling
