//! Test for trace output using expect_test.
//!
//! This test verifies trace event format using snapshot testing.

use expect_test::expect;
use futures::StreamExt;
use futures::channel::mpsc;
use sacp_conductor::trace::{Protocol, TraceEvent, TraceWriter};

/// Strip timestamps from events for stable snapshot testing.
fn strip_timestamps(events: Vec<TraceEvent>) -> Vec<TraceEvent> {
    events
        .into_iter()
        .map(|event| match event {
            TraceEvent::Request(mut r) => {
                r.ts = 0.0;
                TraceEvent::Request(r)
            }
            TraceEvent::Response(mut r) => {
                r.ts = 0.0;
                TraceEvent::Response(r)
            }
            TraceEvent::Notification(mut n) => {
                n.ts = 0.0;
                TraceEvent::Notification(n)
            }
            TraceEvent::Trace(mut t) => {
                t.ts = 0.0;
                TraceEvent::Trace(t)
            }
        })
        .collect()
}

#[tokio::test]
async fn test_trace_events() {
    let (tx, rx) = mpsc::unbounded();
    let mut writer = TraceWriter::new(tx);

    // Write some events
    writer.request(
        Protocol::Acp,
        "client",
        "proxy:0",
        serde_json::json!(1),
        "session/new",
        None,
        serde_json::json!({}),
    );

    writer.response(
        "proxy:0",
        "client",
        serde_json::json!(1),
        false,
        serde_json::json!({"session_id": "test-123"}),
    );

    writer.notification(
        Protocol::Acp,
        "client",
        "proxy:0",
        "session/message",
        Some("test-123".to_string()),
        serde_json::json!({"role": "user", "content": "Hello"}),
    );

    writer.request(
        Protocol::Mcp,
        "agent",
        "proxy:0",
        serde_json::json!(2),
        "tools/list",
        None,
        serde_json::json!({}),
    );

    writer.response(
        "proxy:0",
        "agent",
        serde_json::json!(2),
        false,
        serde_json::json!({"tools": []}),
    );

    // Drop writer to close the channel
    drop(writer);

    // Collect events from receiver and strip timestamps
    let events = strip_timestamps(rx.collect().await);

    expect![[r#"
        [
            Request(
                RequestEvent {
                    ts: 0.0,
                    protocol: Acp,
                    from: "client",
                    to: "proxy:0",
                    id: Number(1),
                    method: "session/new",
                    session: None,
                    params: Object {},
                },
            ),
            Response(
                ResponseEvent {
                    ts: 0.0,
                    from: "proxy:0",
                    to: "client",
                    id: Number(1),
                    is_error: false,
                    payload: Object {
                        "session_id": String("test-123"),
                    },
                },
            ),
            Notification(
                NotificationEvent {
                    ts: 0.0,
                    protocol: Acp,
                    from: "client",
                    to: "proxy:0",
                    method: "session/message",
                    session: Some(
                        "test-123",
                    ),
                    params: Object {
                        "content": String("Hello"),
                        "role": String("user"),
                    },
                },
            ),
            Request(
                RequestEvent {
                    ts: 0.0,
                    protocol: Mcp,
                    from: "agent",
                    to: "proxy:0",
                    id: Number(2),
                    method: "tools/list",
                    session: None,
                    params: Object {},
                },
            ),
            Response(
                ResponseEvent {
                    ts: 0.0,
                    from: "proxy:0",
                    to: "agent",
                    id: Number(2),
                    is_error: false,
                    payload: Object {
                        "tools": Array [],
                    },
                },
            ),
        ]
    "#]]
    .assert_debug_eq(&events);
}
