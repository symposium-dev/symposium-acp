//! Integration tests for JSON-RPC connection builder behavior.
//!
//! These tests verify that multiple handlers can be registered on a connection builder
//! and that requests/notifications are routed correctly based on which
//! handler claims them.

use sacp::link::UntypedLink;
use sacp::util::run_until;
use sacp::{
    Component, JrConnectionCx, JrMessage, JrNotification, JrRequest, JrRequestCx, JrResponse,
    JrResponsePayload,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Test helper to block and wait for a JSON-RPC response.
async fn recv<T: JrResponsePayload + Send>(response: JrResponse<T>) -> Result<T, sacp::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    response.on_receiving_result(async move |result| {
        tx.send(result).map_err(|_| sacp::Error::internal_error())
    })?;
    rx.await.map_err(|_| sacp::Error::internal_error())?
}

// ============================================================================
// Test 1: Multiple handlers with different methods
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FooRequest {
    value: String,
}

impl JrMessage for FooRequest {
    fn method(&self) -> &str {
        "foo"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "foo" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrRequest for FooRequest {
    type Response = FooResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FooResponse {
    result: String,
}

impl JrResponsePayload for FooResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, sacp::Error> {
        serde_json::to_value(self).map_err(sacp::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, sacp::Error> {
        sacp::util::json_cast(&value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BarRequest {
    value: String,
}

impl JrMessage for BarRequest {
    fn method(&self) -> &str {
        "bar"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "bar" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrRequest for BarRequest {
    type Response = BarResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BarResponse {
    result: String,
}

impl JrResponsePayload for BarResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, sacp::Error> {
        serde_json::to_value(self).map_err(sacp::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, sacp::Error> {
        sacp::util::json_cast(&value)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_multiple_handlers_different_methods() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // Chain both handlers
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder()
                .on_receive_request(
                    async |request: FooRequest,
                           request_cx: JrRequestCx<FooResponse>,
                           _connection_cx: JrConnectionCx<UntypedLink>| {
                        request_cx.respond(FooResponse {
                            result: format!("foo: {}", request.value),
                        })
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_request(
                    async |request: BarRequest,
                           request_cx: JrRequestCx<BarResponse>,
                           _connection_cx: JrConnectionCx<UntypedLink>| {
                        request_cx.respond(BarResponse {
                            result: format!("bar: {}", request.value),
                        })
                    },
                    sacp::on_receive_request!(),
                );
            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = UntypedLink::builder();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {e:?}");
                }
            });

            let result = client
                .run_until(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        // Test foo request
                        let foo_response = recv(cx.send_request(FooRequest {
                            value: "test1".to_string(),
                        }))
                        .await
                        .map_err(|e| -> sacp::Error {
                            sacp::util::internal_error(format!("Foo request failed: {e:?}"))
                        })?;
                        assert_eq!(foo_response.result, "foo: test1");

                        // Test bar request
                        let bar_response = recv(cx.send_request(BarRequest {
                            value: "test2".to_string(),
                        }))
                        .await
                        .map_err(|e| -> sacp::Error {
                            sacp::util::internal_error(format!("Bar request failed: {:?}", e))
                        })?;
                        assert_eq!(bar_response.result, "bar: test2");

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);
        })
        .await;
}

// ============================================================================
// Test 2: Handler priority/ordering (first handler gets first chance)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrackRequest {
    value: String,
}

impl JrMessage for TrackRequest {
    fn method(&self) -> &str {
        "track"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "track" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrRequest for TrackRequest {
    type Response = FooResponse;
}

#[tokio::test(flavor = "current_thread")]
async fn test_handler_priority_ordering() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let handled = Arc::new(Mutex::new(Vec::new()));

            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // First handler in chain should get first chance
            let handled_clone1 = handled.clone();
            let handled_clone2 = handled.clone();
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder()
                .on_receive_request(
                    async move |request: TrackRequest,
                                request_cx: JrRequestCx<FooResponse>,
                                _connection_cx: JrConnectionCx<UntypedLink>| {
                        handled_clone1.lock().unwrap().push("handler1".to_string());
                        request_cx.respond(FooResponse {
                            result: format!("handler1: {}", request.value),
                        })
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: TrackRequest,
                                request_cx: JrRequestCx<FooResponse>,
                                _connection_cx: JrConnectionCx<UntypedLink>| {
                        handled_clone2.lock().unwrap().push("handler2".to_string());
                        request_cx.respond(FooResponse {
                            result: format!("handler2: {}", request.value),
                        })
                    },
                    sacp::on_receive_request!(),
                );
            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = UntypedLink::builder();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {:?}", e);
                }
            });

            let result = client
                .run_until(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        let response = recv(cx.send_request(TrackRequest {
                            value: "test".to_string(),
                        }))
                        .await
                        .map_err(|e| {
                            sacp::util::internal_error(format!("Track request failed: {:?}", e))
                        })?;

                        // First handler should have handled it
                        assert_eq!(response.result, "handler1: test");

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify only handler1 was invoked
            let handled_by = handled.lock().unwrap();
            assert_eq!(handled_by.len(), 1);
            assert_eq!(handled_by[0], "handler1");
        })
        .await;
}

// ============================================================================
// Test 3: Fallthrough behavior (handler passes to next)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Method1Request {
    value: String,
}

impl JrMessage for Method1Request {
    fn method(&self) -> &str {
        "method1"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "method1" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrRequest for Method1Request {
    type Response = FooResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Method2Request {
    value: String,
}

impl JrMessage for Method2Request {
    fn method(&self) -> &str {
        "method2"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "method2" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrRequest for Method2Request {
    type Response = FooResponse;
}

#[tokio::test(flavor = "current_thread")]
async fn test_fallthrough_behavior() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let handled = Arc::new(Mutex::new(Vec::new()));

            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // Handler1 only handles "method1", Handler2 only handles "method2"
            let handled_clone1 = handled.clone();
            let handled_clone2 = handled.clone();
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder()
                .on_receive_request(
                    async move |request: Method1Request,
                                request_cx: JrRequestCx<FooResponse>,
                                _connection_cx: JrConnectionCx<UntypedLink>| {
                        handled_clone1.lock().unwrap().push("method1".to_string());
                        request_cx.respond(FooResponse {
                            result: format!("method1: {}", request.value),
                        })
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: Method2Request,
                                request_cx: JrRequestCx<FooResponse>,
                                _connection_cx: JrConnectionCx<UntypedLink>| {
                        handled_clone2.lock().unwrap().push("method2".to_string());
                        request_cx.respond(FooResponse {
                            result: format!("method2: {}", request.value),
                        })
                    },
                    sacp::on_receive_request!(),
                );
            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = UntypedLink::builder();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {:?}", e);
                }
            });

            let result = client
                .run_until(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        // Send method2 - should fallthrough handler1 to handler2
                        let response = recv(cx.send_request(Method2Request {
                            value: "fallthrough".to_string(),
                        }))
                        .await
                        .map_err(|e| {
                            sacp::util::internal_error(format!("Method2 request failed: {:?}", e))
                        })?;

                        assert_eq!(response.result, "method2: fallthrough");

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify only method2 was handled (handler1 passed through)
            let handled_methods = handled.lock().unwrap();
            assert_eq!(handled_methods.len(), 1);
            assert_eq!(handled_methods[0], "method2");
        })
        .await;
}

// ============================================================================
// Test 4: No handler claims request
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn test_no_handler_claims() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // Handler that only handles "foo"
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder().on_receive_request(
                async |request: FooRequest,
                       request_cx: JrRequestCx<FooResponse>,
                       _connection_cx: JrConnectionCx<UntypedLink>| {
                    request_cx.respond(FooResponse {
                        result: format!("foo: {}", request.value),
                    })
                },
                sacp::on_receive_request!(),
            );
            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = UntypedLink::builder();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {:?}", e);
                }
            });

            let result = client
                .run_until(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        // Send "bar" request which no handler claims
                        let response_result = recv(cx.send_request(BarRequest {
                            value: "unclaimed".to_string(),
                        }))
                        .await;

                        // Should get an error (method not found)
                        assert!(response_result.is_err());

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);
        })
        .await;
}

// ============================================================================
// Test 5: Handler can claim notifications
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EventNotification {
    event: String,
}

impl JrMessage for EventNotification {
    fn method(&self) -> &str {
        "event"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "event" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrNotification for EventNotification {}

#[tokio::test(flavor = "current_thread")]
async fn test_handler_claims_notification() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let events = Arc::new(Mutex::new(Vec::new()));

            let (client_writer, server_reader) = tokio::io::duplex(1024);
            let (server_writer, client_reader) = tokio::io::duplex(1024);

            let server_reader = server_reader.compat();
            let server_writer = server_writer.compat_write();
            let client_reader = client_reader.compat();
            let client_writer = client_writer.compat_write();

            // EventHandler claims notifications
            let events_clone = events.clone();
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder().on_receive_notification(
                async move |notification: EventNotification,
                            _notification_cx: JrConnectionCx<UntypedLink>| {
                    events_clone.lock().unwrap().push(notification.event);
                    Ok(())
                },
                sacp::on_receive_notification!(),
            );
            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = UntypedLink::builder();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {:?}", e);
                }
            });

            let result = client
                .run_until(
                    client_transport,
                    async |cx| -> std::result::Result<(), sacp::Error> {
                        cx.send_notification(EventNotification {
                            event: "test_event".to_string(),
                        })
                        .map_err(|e| {
                            sacp::util::internal_error(format!(
                                "Failed to send notification: {:?}",
                                e
                            ))
                        })?;

                        // Give server time to process
                        tokio::time::sleep(Duration::from_millis(100)).await;

                        Ok(())
                    },
                )
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            let received_events = events.lock().unwrap();
            assert_eq!(received_events.len(), 1);
            assert_eq!(received_events[0], "test_event");
        })
        .await;
}

// ============================================================================
// Test 6: JrConnectionBuilder implements Component
// ============================================================================

#[tokio::test]
async fn test_connection_builder_as_component() -> Result<(), sacp::Error> {
    // Create duplex streams
    let (server_stream, client_stream) = tokio::io::duplex(8192);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let (client_read, client_write) = tokio::io::split(client_stream);

    // Create a connection builder (server side)
    let server_builder = UntypedLink::builder().on_receive_request(
        async |request: FooRequest,
               request_cx: JrRequestCx<FooResponse>,
               _cx: JrConnectionCx<UntypedLink>| {
            request_cx.respond(FooResponse {
                result: format!("component: {}", request.value),
            })
        },
        sacp::on_receive_request!(),
    );

    // Create ByteStreams for both sides
    let server_transport =
        sacp::ByteStreams::new(server_write.compat_write(), server_read.compat());
    let client_transport =
        sacp::ByteStreams::new(client_write.compat_write(), client_read.compat());

    // Use JrConnectionBuilder as a Component via run_until
    run_until(
        // This uses Component::serve on JrConnectionBuilder
        Component::<UntypedLink>::serve(server_builder, server_transport),
        async move {
            // Client side
            UntypedLink::builder()
                .run_until(client_transport, async |cx| {
                    let response = recv(cx.send_request(FooRequest {
                        value: "test".to_string(),
                    }))
                    .await?;

                    assert_eq!(response.result, "component: test");
                    Ok(())
                })
                .await
        },
    )
    .await
}
