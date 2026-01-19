// Types re-exported from crate root
use std::collections::HashMap;

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures_concurrency::stream::StreamExt as _;
use fxhash::FxHashMap;
use uuid::Uuid;

use crate::MessageCx;
use crate::RoleId;
use crate::UntypedMessage;
use crate::jsonrpc::ConnectionTo;
use crate::jsonrpc::HandleMessageFrom;
use crate::jsonrpc::ReplyMessage;
use crate::jsonrpc::Responder;
use crate::jsonrpc::ResponseRouter;
use crate::jsonrpc::dynamic_handler::DynHandleMessageFrom;
use crate::jsonrpc::dynamic_handler::DynamicHandlerMessage;

use crate::role::Role;

use super::Handled;

struct PendingReply {
    method: String,
    role_id: RoleId,
    sender: oneshot::Sender<crate::jsonrpc::ResponsePayload>,
}

/// Incoming protocol actor: The central dispatch loop for a connection.
///
/// This actor handles JSON-RPC protocol semantics:
/// - Routes responses to pending request awaiters
/// - Routes requests/notifications to registered handlers
/// - Converts jsonrpcmsg::Request to UntypedMessage for handlers
/// - Manages reply subscriptions from outgoing requests
///
/// This is the protocol layer - it has no knowledge of how messages arrived.
///
/// The type parameter `MyRole` is the role of this endpoint (e.g., `Agent`).
/// Messages are received from `MyRole::Counterpart` (e.g., `Client`).
pub(super) async fn incoming_protocol_actor<Counterpart: Role>(
    counterpart: Counterpart,
    connection: &ConnectionTo<Counterpart>,
    transport_rx: mpsc::UnboundedReceiver<Result<jsonrpcmsg::Message, crate::Error>>,
    dynamic_handler_rx: mpsc::UnboundedReceiver<DynamicHandlerMessage<Counterpart>>,
    reply_rx: mpsc::UnboundedReceiver<ReplyMessage>,
    mut handler: impl HandleMessageFrom<Counterpart>,
) -> Result<(), crate::Error> {
    let mut my_rx = transport_rx
        .map(IncomingProtocolMsg::Transport)
        .merge(dynamic_handler_rx.map(IncomingProtocolMsg::DynamicHandler))
        .merge(reply_rx.map(IncomingProtocolMsg::Reply));

    let mut dynamic_handlers: FxHashMap<Uuid, Box<dyn DynHandleMessageFrom<Counterpart>>> =
        FxHashMap::default();
    let mut pending_messages: Vec<MessageCx> = vec![];

    // Map from request ID to (method, sender) for response dispatch.
    // Keys are JSON values because jsonrpcmsg::Id doesn't implement Eq.
    // The method is stored to allow routing responses through typed handlers.
    let mut pending_replies: HashMap<serde_json::Value, PendingReply> = HashMap::new();

    while let Some(message_result) = my_rx.next().await {
        tracing::trace!(message = ?message_result, actor = "incoming_protocol_actor");
        match message_result {
            IncomingProtocolMsg::Reply(message) => match message {
                ReplyMessage::Subscribe {
                    id,
                    role_id,
                    method,
                    sender,
                } => {
                    tracing::trace!(?id, %method, "incoming_actor: subscribing to response");
                    let id = serde_json::to_value(&id).unwrap();
                    pending_replies.insert(
                        id,
                        PendingReply {
                            method,
                            role_id,
                            sender,
                        },
                    );
                }
            },

            IncomingProtocolMsg::DynamicHandler(message) => match message {
                DynamicHandlerMessage::AddDynamicHandler(uuid, mut handler) => {
                    // Before adding the new handler, give it a chance to process
                    // any pending messages.
                    let mut new_pending_messages = vec![];
                    for pending_message in pending_messages {
                        tracing::trace!(method = pending_message.method(), handler = ?handler.dyn_describe_chain(), "Retrying message");
                        match handler
                            .dyn_handle_message_from(pending_message, connection.clone())
                            .await?
                        {
                            Handled::Yes => {
                                tracing::trace!("Message handled");
                            }
                            Handled::No {
                                message: m,
                                retry: _,
                            } => {
                                tracing::trace!(method = m.method(), handler = ?handler.dyn_describe_chain(), "Message not handled");
                                new_pending_messages.push(m);
                            }
                        }
                    }
                    pending_messages = new_pending_messages;

                    // Add handler so it will be used for future incoming messages.
                    dynamic_handlers.insert(uuid, handler);
                }
                DynamicHandlerMessage::RemoveDynamicHandler(uuid) => {
                    dynamic_handlers.remove(&uuid);
                }
            },

            IncomingProtocolMsg::Transport(message) => match message {
                Ok(message) => match message {
                    jsonrpcmsg::Message::Request(request) => {
                        tracing::trace!(method = %request.method, id = ?request.id, "Handling request");
                        let message_cx = message_cx_from_request(connection, request);
                        dispatch_message_cx(
                            counterpart.clone(),
                            connection,
                            message_cx,
                            &mut dynamic_handlers,
                            &mut handler,
                            &mut pending_messages,
                        )
                        .await?
                    }
                    jsonrpcmsg::Message::Response(response) => {
                        tracing::trace!(id = ?response.id, has_result = response.result.is_some(), has_error = response.error.is_some(), "Handling response");
                        if let Some(id) = response.id {
                            let result = if let Some(value) = response.result {
                                Ok(value)
                            } else if let Some(error) = response.error {
                                // Convert jsonrpcmsg::Error to crate::Error
                                Err(crate::Error::new(error.code, error.message).data(error.data))
                            } else {
                                // Response with neither result nor error - treat as null result
                                Ok(serde_json::Value::Null)
                            };

                            let id_json = serde_json::to_value(&id).unwrap();
                            if let Some(pending_reply) = pending_replies.remove(&id_json) {
                                // Route the response through the handler chain
                                let message_cx =
                                    message_cx_from_response(id, pending_reply, result);
                                dispatch_message_cx(
                                    counterpart.clone(),
                                    connection,
                                    message_cx,
                                    &mut dynamic_handlers,
                                    &mut handler,
                                    &mut pending_messages,
                                )
                                .await?;
                            } else {
                                tracing::warn!(
                                    ?id,
                                    "incoming_actor: received response for unknown id, no subscriber found"
                                );
                            }
                        }
                    }
                },
                Err(error) => {
                    // Parse error from transport - send error notification back to remote
                    tracing::warn!(?error, "Transport parse error, sending error notification");
                    connection.send_error_notification(error)?;
                }
            },
        }
    }
    Ok(())
}

#[derive(Debug)]
enum IncomingProtocolMsg<Counterpart: Role> {
    Transport(Result<jsonrpcmsg::Message, crate::Error>),
    DynamicHandler(DynamicHandlerMessage<Counterpart>),
    Reply(ReplyMessage),
}

/// Dispatches a JSON-RPC request to the handler.
/// Report an error back to the server if it does not get handled.
fn message_cx_from_request<Counterpart: Role>(
    json_rpc_cx: &ConnectionTo<Counterpart>,
    request: jsonrpcmsg::Request,
) -> MessageCx {
    let message = UntypedMessage::new(&request.method, &request.params).expect("well-formed JSON");

    let message_cx = match &request.id {
        Some(id) => MessageCx::Request(
            message,
            Responder::new(
                json_rpc_cx.message_tx.clone(),
                request.method.clone(),
                id.clone(),
            ),
        ),
        None => MessageCx::Notification(message),
    };

    message_cx
}

/// Dispatches a JSON-RPC response through the handler chain.
///
/// This allows handlers to intercept and process responses before they reach
/// the awaiting code. The default behavior is to forward the response to the
/// local awaiter via the oneshot channel.
fn message_cx_from_response(
    id: jsonrpcmsg::Id,
    pending_reply: PendingReply,
    result: Result<serde_json::Value, crate::Error>,
) -> MessageCx {
    let PendingReply {
        method,
        role_id,
        sender,
    } = pending_reply;

    // Create a MessageCx::Response with a ResponseRouter that routes to the oneshot
    let response_cx = ResponseRouter::new(method.clone(), id.clone(), role_id, sender);
    MessageCx::Response(result, response_cx)
}

#[tracing::instrument(
    skip(connection, message_cx, dynamic_handlers, handler, pending_messages),
    fields(method = message_cx.method()),
    level = "trace",
)]
async fn dispatch_message_cx<Counterpart: Role>(
    counterpart: Counterpart,
    connection: &ConnectionTo<Counterpart>,
    mut message_cx: MessageCx,
    dynamic_handlers: &mut FxHashMap<Uuid, Box<dyn DynHandleMessageFrom<Counterpart>>>,
    handler: &mut impl HandleMessageFrom<Counterpart>,
    pending_messages: &mut Vec<MessageCx>,
) -> Result<(), crate::Error> {
    tracing::trace!(?message_cx, "dispatch_message_cx");

    let mut retry_any = false;

    let id = message_cx.id();
    let method = message_cx.method().to_string();

    // First, apply the handlers given by the user.
    tracing::trace!(handler = ?handler.describe_chain(), "Attempting handler chain");
    match handler
        .handle_message_from(message_cx, connection.clone())
        .await?
    {
        Handled::Yes => {
            tracing::trace!(?method, ?id, handler = ?handler.describe_chain(), "Handler accepted message");
            return Ok(());
        }

        Handled::No { message: m, retry } => {
            tracing::trace!(?method, ?id, handler = ?handler.describe_chain(), "Handler declined message");
            message_cx = m;
            retry_any |= retry;
        }
    }

    // Next, apply any dynamic handlers.
    for dynamic_handler in dynamic_handlers.values_mut() {
        tracing::trace!(handler = ?dynamic_handler.dyn_describe_chain(), "Attempting dynamic handler");
        match dynamic_handler
            .dyn_handle_message_from(message_cx, connection.clone())
            .await?
        {
            Handled::Yes => {
                tracing::trace!(?method, ?id, handler = ?dynamic_handler.dyn_describe_chain(), "Dynamic handler accepted message");
                return Ok(());
            }

            Handled::No { message: m, retry } => {
                tracing::trace!(?method, ?id, handler = ?dynamic_handler.dyn_describe_chain(),  "Dynamic handler declined message");
                retry_any |= retry;
                message_cx = m;
            }
        }
    }

    // Finally, apply the default handler for the role.
    tracing::trace!(role = ?counterpart, "Attempting default handler");
    match counterpart
        .default_handle_message_from(message_cx, connection.clone())
        .await?
    {
        Handled::Yes => {
            tracing::trace!(?method, handler = "default", "Role accepted message");
            return Ok(());
        }
        Handled::No { message: m, retry } => {
            tracing::trace!(?method, handler = "default", "Role declined message");
            message_cx = m;
            retry_any |= retry;
        }
    }

    // If the message was never handled, check whether the retry flag was set.
    // If so, enqueue it for later processing. Else, reject it.
    if retry_any {
        tracing::debug!(
            ?method,
            "Retrying message as new dynamic handlers are added"
        );
        pending_messages.push(message_cx);
        Ok(())
    } else {
        match message_cx {
            MessageCx::Request(..) | MessageCx::Notification(_) => {
                tracing::info!(?method, "Rejecting message with error, no handler");
                let method = message_cx.method().to_string();
                message_cx.respond_with_error(
                    crate::Error::method_not_found().data(method),
                    connection.clone(),
                )
            }
            MessageCx::Response(result, response_cx) => {
                tracing::trace!(?method, "Forwarding response");
                response_cx.respond_with_result(result)
            }
        }
    }
}
