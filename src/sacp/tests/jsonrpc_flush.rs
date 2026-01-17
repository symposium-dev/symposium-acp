//! Integration test for flush() mechanism ensuring message ordering.

use futures::{AsyncRead, AsyncWrite};
use sacp::link::UntypedLink;
use sacp::{JrConnectionCx, JrMessage, JrNotification, JrRequest, JrRequestCx, JrResponsePayload};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Helper to set up a client-server pair for testing.
fn setup_test_streams() -> (
    impl AsyncRead,
    impl AsyncWrite,
    impl AsyncRead,
    impl AsyncWrite,
) {
    let (client_writer, server_reader) = tokio::io::duplex(1024);
    let (server_writer, client_reader) = tokio::io::duplex(1024);

    let server_reader = server_reader.compat();
    let server_writer = server_writer.compat_write();
    let client_reader = client_reader.compat();
    let client_writer = client_writer.compat_write();

    (server_reader, server_writer, client_reader, client_writer)
}

/// Test notification for tracking order
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestNotification {
    id: u32,
    message: String,
}

impl JrMessage for TestNotification {
    fn method(&self) -> &str {
        "testNotification"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "testNotification" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrNotification for TestNotification {}

/// A test request that triggers notifications before responding
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptRequest {
    prompt: String,
}

impl JrMessage for PromptRequest {
    fn method(&self) -> &str {
        "prompt"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "prompt" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrRequest for PromptRequest {
    type Response = PromptResponse;
}

/// Response to prompt request
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptResponse {
    content: Vec<String>,
    #[serde(rename = "stopReason")]
    stop_reason: Option<String>,
}

impl JrResponsePayload for PromptResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, sacp::Error> {
        serde_json::to_value(self).map_err(sacp::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, sacp::Error> {
        sacp::util::json_cast(&value)
    }
}

/// Session update notification (similar to ACP session/update)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionUpdate {
    status: u32,
}

impl JrMessage for SessionUpdate {
    fn method(&self) -> &str {
        "session/update"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        sacp::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Option<Result<Self, sacp::Error>> {
        if method != "session/update" {
            return None;
        }
        Some(sacp::util::json_cast(params))
    }
}

impl JrNotification for SessionUpdate {}

#[tokio::test(flavor = "current_thread")]
async fn flush_ensures_notifications_are_sent_before_response() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let received = Arc::new(Mutex::new(Vec::new()));
            let received_clone = received.clone();

            let (server_reader, server_writer, client_reader, client_writer) = setup_test_streams();

            // Server tracks received messages in order
            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder().on_receive_request(
                {
                    async move |_req: PromptRequest,
                                request_cx: JrRequestCx<PromptResponse>,
                                cx: JrConnectionCx<UntypedLink>| {
                        // Send multiple session/update notifications
                        for i in 0..5 {
                            cx.send_notification(SessionUpdate { status: i }).unwrap();
                        }

                        // CRITICAL: flush to ensure notifications are sent before response
                        cx.flush().await.unwrap();

                        // Return response
                        request_cx.respond(PromptResponse {
                            content: vec!["done".to_string()],
                            stop_reason: None,
                        })
                    }
                },
                sacp::on_receive_request!(),
            );

            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = UntypedLink::builder().on_receive_notification(
                {
                    let received = received_clone.clone();
                    async move |notification: SessionUpdate, _cx: JrConnectionCx<UntypedLink>| {
                        received
                            .lock()
                            .unwrap()
                            .push(("notification".to_string(), notification.status));
                        Ok(())
                    }
                },
                sacp::on_receive_notification!(),
            );

            // Spawn the server in the background
            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {:?}", e);
                }
            });

            // Use the client to send a prompt request
            let result = client
                .connect_to(client_transport)
                .expect("Failed to connect to transport")
                .run_until(async |cx| -> Result<(), sacp::Error> {
                    let request = PromptRequest {
                        prompt: "test".to_string(),
                    };

                    // Send request
                    let response = cx.send_request(request);

                    // Wait for response (non-blocking using callback)
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    response.on_receiving_result(async move |result| {
                        let _ = tx.send(result);
                        Ok(())
                    })?;

                    let _response = rx.await.unwrap().unwrap();

                    // Give time for any pending notifications
                    tokio::time::sleep(Duration::from_millis(50)).await;

                    Ok(())
                })
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify: all notifications should be received before the response handler completes
            let messages = received.lock().unwrap();
            // We should have received all 5 notifications
            assert_eq!(
                messages.len(),
                5,
                "Expected 5 notifications, got {}",
                messages.len()
            );
            // Verify all are notifications with status 0-4
            for (i, (msg_type, status)) in messages.iter().enumerate() {
                assert_eq!(msg_type, "notification");
                assert_eq!(*status, i as u32);
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flush_with_no_messages_returns_immediately() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let (server_reader, server_writer, client_reader, client_writer) = setup_test_streams();

            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder();

            let client_transport = sacp::ByteStreams::new(client_writer, client_reader);
            let client = UntypedLink::builder();

            tokio::task::spawn_local(async move {
                if let Err(e) = server.serve(server_transport).await {
                    eprintln!("Server error: {:?}", e);
                }
            });

            let result = client
                .connect_to(client_transport)
                .expect("Failed to connect to transport")
                .run_until(async |cx| -> Result<(), sacp::Error> {
                    // No messages sent, just call flush
                    let start = Instant::now();
                    cx.flush().await.unwrap();
                    let elapsed = start.elapsed();

                    // Should return immediately (less than 50ms)
                    assert!(elapsed < Duration::from_millis(50));

                    Ok(())
                })
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flush_with_interleaved_notifications() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let received = Arc::new(Mutex::new(Vec::new()));
            let received_clone = received.clone();

            let (server_reader, server_writer, client_reader, client_writer) = setup_test_streams();

            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder().on_receive_notification(
                {
                    let received = received_clone.clone();
                    async move |notification: TestNotification, _cx: JrConnectionCx<UntypedLink>| {
                        received.lock().unwrap().push(notification.id);
                        Ok(())
                    }
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
                .connect_to(client_transport)
                .expect("Failed to connect to transport")
                .run_until(async |cx| -> Result<(), sacp::Error> {
                    // Send notification A (id=1)
                    cx.send_notification(TestNotification {
                        id: 1,
                        message: "A".to_string(),
                    })?;

                    // flush to ensure A is sent
                    cx.flush().await?;

                    // Send notification B (id=2)
                    cx.send_notification(TestNotification {
                        id: 2,
                        message: "B".to_string(),
                    })?;

                    // flush again
                    cx.flush().await?;

                    // Send notification C (id=3)
                    cx.send_notification(TestNotification {
                        id: 3,
                        message: "C".to_string(),
                    })?;

                    // Final flush
                    cx.flush().await?;

                    // Give time for all notifications to be processed
                    tokio::time::sleep(Duration::from_millis(50)).await;

                    Ok(())
                })
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify order: 1, 2, 3
            let messages = received.lock().unwrap();
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0], 1);
            assert_eq!(messages[1], 2);
            assert_eq!(messages[2], 3);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flush_multiple_messages_in_batch() {
    use tokio::task::LocalSet;

    let local = LocalSet::new();

    local
        .run_until(async {
            let received = Arc::new(Mutex::new(Vec::new()));
            let received_clone = received.clone();

            let (server_reader, server_writer, client_reader, client_writer) = setup_test_streams();

            let server_transport = sacp::ByteStreams::new(server_writer, server_reader);
            let server = UntypedLink::builder().on_receive_notification(
                {
                    let received = received_clone.clone();
                    async move |notification: TestNotification, _cx: JrConnectionCx<UntypedLink>| {
                        received.lock().unwrap().push(notification.id);
                        Ok(())
                    }
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
                .connect_to(client_transport)
                .expect("Failed to connect to transport")
                .run_until(async |cx| -> Result<(), sacp::Error> {
                    // Send multiple notifications rapidly
                    for i in 0..10 {
                        cx.send_notification(TestNotification {
                            id: i,
                            message: format!("msg{}", i),
                        })?;
                    }

                    // Single flush should ensure all are sent
                    cx.flush().await?;

                    // Give time for all notifications to be processed
                    tokio::time::sleep(Duration::from_millis(50)).await;

                    Ok(())
                })
                .await;

            assert!(result.is_ok(), "Test failed: {:?}", result);

            // Verify all 10 messages were received
            let messages = received.lock().unwrap();
            assert_eq!(messages.len(), 10);
            for i in 0..10 {
                assert_eq!(messages[i], i as u32);
            }
        })
        .await;
}
