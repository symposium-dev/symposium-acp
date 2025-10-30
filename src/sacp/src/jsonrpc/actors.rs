use std::collections::HashMap;
use std::panic::Location;
use std::pin::pin;

// Types re-exported from crate root
use futures::AsyncBufReadExt as _;
use futures::AsyncRead;
use futures::AsyncWrite;
use futures::AsyncWriteExt as _;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::io::BufReader;
use futures::stream::FusedStream;
use futures::stream::FuturesUnordered;
use uuid::Uuid;

use crate::MessageAndCx;
use crate::UntypedMessage;
use crate::jsonrpc::JrConnectionCx;
use crate::jsonrpc::JrHandler;
use crate::jsonrpc::JrRequestCx;
use crate::jsonrpc::OutgoingMessage;
use crate::jsonrpc::ReplyMessage;

use super::Handled;

pub(crate) struct Task {
    pub location: &'static Location<'static>,
    pub future: BoxFuture<'static, Result<(), crate::Error>>,
}

/// The "task actor" manages other tasks
pub(super) async fn task_actor(
    mut task_rx: mpsc::UnboundedReceiver<Task>,
) -> Result<(), crate::Error> {
    let mut futures = FuturesUnordered::new();

    loop {
        // If we have no futures to run, wait until we do.
        if futures.is_empty() {
            match task_rx.next().await {
                Some(task) => futures.push(execute_task(task)),
                None => return Ok(()),
            }
            continue;
        }

        // If there are no more tasks coming in, just drain our queue and return.
        if task_rx.is_terminated() {
            while let Some(result) = futures.next().await {
                result?;
            }
            return Ok(());
        }

        // Otherwise, run futures until we get a request for a new task.
        futures::select! {
            result = futures.next() => if let Some(result) = result {
                result?;
            },

            task = task_rx.next() => {
                if let Some(task) = task {
                    futures.push(execute_task(task));
                }
            }
        }
    }
}

async fn execute_task(task: Task) -> Result<(), crate::Error> {
    let Task { location, future } = task;
    future.await.map_err(|err| {
        let data = err.data.clone();
        err.with_data(serde_json::json! {
            {
                "spawned_at": format!("{}:{}:{}", location.file(), location.line(), location.column()),
                "data": data,
            }
        })
    })
}

/// The "reply actor" manages a queue of pending replies.
pub(super) async fn reply_actor(
    mut reply_rx: mpsc::UnboundedReceiver<ReplyMessage>,
) -> Result<(), crate::Error> {
    // Map from the `id` to a oneshot sender where we should send the value.
    let mut map = HashMap::new();

    while let Some(message) = reply_rx.next().await {
        match message {
            ReplyMessage::Subscribe(id, message_tx) => {
                // total hack: id's don't implement Eq
                tracing::trace!(?id, "reply_actor: subscribing to response");
                let id = serde_json::to_value(&id).unwrap();
                map.insert(id, message_tx);
            }
            ReplyMessage::Dispatch(id, value) => {
                let id_debug = &id;
                let is_ok = value.is_ok();
                tracing::trace!(?id_debug, is_ok, "reply_actor: dispatching response");
                let id = serde_json::to_value(&id).unwrap();
                if let Some(message_tx) = map.remove(&id) {
                    // If the receiver is no longer interested in the reply,
                    // that's ok with us.
                    let result = message_tx.send(value);
                    if result.is_err() {
                        tracing::warn!(
                            ?id,
                            "reply_actor: failed to send response, receiver dropped"
                        );
                    } else {
                        tracing::trace!(
                            ?id,
                            "reply_actor: successfully dispatched response to receiver"
                        );
                    }
                } else {
                    tracing::warn!(
                        ?id,
                        "reply_actor: received response for unknown id, no subscriber found"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Parsing incoming messages from `incoming_bytes`.
/// Each message will be dispatched to the appropriate layer.
///
/// # Parameters
/// - `json_rpc_cx`: The JSON-RPC context.
/// - `incoming_bytes`: The incoming bytes.
/// - `reply_tx`: The reply sender.
/// - `mut cancellation_tx`: cancellation signal; when the rx side of this channel is dropped, the actor will terminate
/// - `layers`: The layers.
///
/// # Returns
/// - `Result<(), crate::Error>`: an error if something unrecoverable occurred
pub(super) async fn incoming_actor(
    connection_name: &Option<String>,
    json_rpc_cx: &JrConnectionCx,
    incoming_bytes: impl AsyncRead,
    reply_tx: mpsc::UnboundedSender<ReplyMessage>,
    mut handler: impl JrHandler,
) -> Result<(), crate::Error> {
    let incoming_bytes = pin!(incoming_bytes);
    let buffered_incoming_bytes = BufReader::new(incoming_bytes);
    let mut incoming_lines = buffered_incoming_bytes.lines();
    while let Some(line) = incoming_lines.next().await {
        let line = line.map_err(crate::Error::into_internal_error)?;
        tracing::trace!(message = %line, "Received JSON-RPC message");
        let message: Result<jsonrpcmsg::Message, _> = serde_json::from_str(&line);
        match message {
            Ok(msg) => match msg {
                jsonrpcmsg::Message::Request(request) => {
                    tracing::trace!(method = %request.method, id = ?request.id, "Handling request");
                    dispatch_request(connection_name, json_rpc_cx, request, &mut handler).await?
                }
                jsonrpcmsg::Message::Response(response) => {
                    tracing::trace!(id = ?response.id, has_result = response.result.is_some(), has_error = response.error.is_some(), "Handling response");
                    if let Some(id) = response.id {
                        if let Some(value) = response.result {
                            reply_tx
                                .unbounded_send(ReplyMessage::Dispatch(id, Ok(value)))
                                .map_err(crate::Error::into_internal_error)?;
                        } else if let Some(error) = response.error {
                            // Convert jsonrpcmsg::Error to crate::Error
                            let acp_error = crate::Error {
                                code: error.code,
                                message: error.message,
                                data: error.data,
                            };
                            reply_tx
                                .unbounded_send(ReplyMessage::Dispatch(id, Err(acp_error)))
                                .map_err(crate::Error::into_internal_error)?;
                        }
                    }
                }
            },
            Err(_) => {
                json_rpc_cx.send_error_notification(crate::Error::parse_error())?;
            }
        }
    }
    Ok(())
}

/// Dispatches a JSON-RPC request to the handler.
/// Report an error back to the server if it does not get handled.
async fn dispatch_request(
    connection_name: &Option<String>,
    json_rpc_cx: &JrConnectionCx,
    request: jsonrpcmsg::Request,
    handler: &mut impl JrHandler,
) -> Result<(), crate::Error> {
    tracing::debug!(
        ?request,
        "handler_type" = ?handler.describe_chain(),
        connection_name,
        "dispatch_request"
    );

    let message = UntypedMessage::new(&request.method, &request.params).expect("well-formed JSON");

    let message_cx = match &request.id {
        Some(id) => MessageAndCx::Request(
            message,
            JrRequestCx::new(json_rpc_cx, request.method.clone(), id.clone()),
        ),
        None => MessageAndCx::Notification(message, json_rpc_cx.clone()),
    };

    match handler.handle_message(message_cx).await? {
        Handled::Yes => {
            tracing::debug!(method = request.method, ?request.id, "Handler reported: Handled::Yes");
        }
        Handled::No(request_cx) => {
            tracing::debug!(method = ?request.method, ?request.id, "Handler reported: Handled::No, sending method_not_found");
            request_cx.respond_with_error(crate::Error::method_not_found())?;
        }
    }

    Ok(())
}

/// Actor processing outgoing messages and serializing them onto the transport.
///
/// # Parameters
///
/// * `outgoing_rx`: Receiver for outgoing messages.
/// * `reply_tx`: Sender for reply messages.
/// * `outgoing_bytes`: AsyncWrite for sending messages.
pub(super) async fn outgoing_actor(
    connection_name: &Option<String>,
    mut outgoing_rx: mpsc::UnboundedReceiver<OutgoingMessage>,
    reply_tx: mpsc::UnboundedSender<ReplyMessage>,
    outgoing_bytes: impl AsyncWrite,
) -> Result<(), crate::Error> {
    let mut outgoing_bytes = pin!(outgoing_bytes);

    while let Some(message) = outgoing_rx.next().await {
        tracing::debug!(connection_name, ?message, "outgoing_actor");

        // Create the message to be sent over the transport
        let json_rpc_message = match message {
            OutgoingMessage::Request {
                method,
                params,
                response_tx: response_rx,
            } => {
                // Generate a fresh UUID to use for the request id
                let uuid = Uuid::new_v4();
                let id = jsonrpcmsg::Id::String(uuid.to_string());

                // Record where the reply should be sent once it arrives.
                reply_tx
                    .unbounded_send(ReplyMessage::Subscribe(id.clone(), response_rx))
                    .map_err(crate::Error::into_internal_error)?;

                jsonrpcmsg::Message::Request(jsonrpcmsg::Request::new_v2(method, params, Some(id)))
            }
            OutgoingMessage::Notification { method, params } => {
                jsonrpcmsg::Message::Request(jsonrpcmsg::Request::new_v2(method, params, None))
            }
            OutgoingMessage::Response {
                id,
                response: Ok(value),
            } => {
                tracing::debug!(?id, "Sending success response");
                jsonrpcmsg::Message::Response(jsonrpcmsg::Response::success_v2(value, Some(id)))
            }
            OutgoingMessage::Response {
                id,
                response: Err(error),
            } => {
                tracing::warn!(?id, ?error, "Sending error response");
                // Convert crate::Error to jsonrpcmsg::Error
                let jsonrpc_error = jsonrpcmsg::Error {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                };
                jsonrpcmsg::Message::Response(jsonrpcmsg::Response::error_v2(
                    jsonrpc_error,
                    Some(id),
                ))
            }
            OutgoingMessage::Error { error } => {
                // Convert crate::Error to jsonrpcmsg::Error
                let jsonrpc_error = jsonrpcmsg::Error {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                };
                jsonrpcmsg::Message::Response(jsonrpcmsg::Response::error_v2(jsonrpc_error, None))
            }
        };

        match serde_json::to_vec(&json_rpc_message) {
            Ok(mut bytes) => {
                if let Ok(msg_str) = std::str::from_utf8(&bytes) {
                    tracing::trace!(message = %msg_str, "Sending JSON-RPC message");
                }
                bytes.push('\n' as u8);
                outgoing_bytes
                    .write_all(&bytes)
                    .await
                    .map_err(crate::Error::into_internal_error)?;
            }

            Err(serialization_error) => {
                match json_rpc_message {
                    jsonrpcmsg::Message::Request(_request) => {
                        // If we failed to serialize a request,
                        // just ignore it.
                        //
                        // Q: (Maybe it'd be nice to "reply" with an error?)
                        tracing::error!(
                            ?serialization_error,
                            "Failed to serialize request, ignoring"
                        );
                    }
                    jsonrpcmsg::Message::Response(response) => {
                        // If we failed to serialize a *response*,
                        // send an error in response.
                        tracing::error!(?serialization_error, id = ?response.id, "Failed to serialize response, sending internal_error instead");
                        // Convert crate::Error to jsonrpcmsg::Error
                        let acp_error = crate::Error::internal_error();
                        let jsonrpc_error = jsonrpcmsg::Error {
                            code: acp_error.code,
                            message: acp_error.message,
                            data: acp_error.data,
                        };
                        outgoing_bytes
                            .write_all(
                                &serde_json::to_vec(&jsonrpcmsg::Response::error(
                                    jsonrpc_error,
                                    response.id,
                                ))
                                .unwrap(),
                            )
                            .await
                            .map_err(crate::Error::into_internal_error)?;
                    }
                }
            }
        };
    }
    Ok(())
}
