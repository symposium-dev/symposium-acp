// Types re-exported from crate root
use std::collections::HashMap;

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures_concurrency::stream::StreamExt as _;
use fxhash::FxHashMap;
use uuid::Uuid;

use crate::MessageCx;
use crate::UntypedMessage;
use crate::jsonrpc::JrConnectionCx;
use crate::jsonrpc::JrMessageHandler;
use crate::jsonrpc::JrRequestCx;
use crate::jsonrpc::JrResponseCx;
use crate::jsonrpc::ReplyMessage;
use crate::jsonrpc::dynamic_handler::DynamicHandler;
use crate::jsonrpc::dynamic_handler::DynamicHandlerMessage;
use crate::link::JrLink;

use super::Handled;

/// Incoming protocol actor: The central dispatch loop for a connection.
///
/// This actor handles JSON-RPC protocol semantics:
/// - Routes responses to pending request awaiters
/// - Routes requests/notifications to registered handlers
/// - Converts jsonrpcmsg::Request to UntypedMessage for handlers
/// - Manages reply subscriptions from outgoing requests
///
/// This is the protocol layer - it has no knowledge of how messages arrived.
pub(super) async fn incoming_protocol_actor<Link: JrLink>(
    json_rpc_cx: &JrConnectionCx<Link>,
    transport_rx: mpsc::UnboundedReceiver<Result<jsonrpcmsg::Message, crate::Error>>,
    dynamic_handler_rx: mpsc::UnboundedReceiver<DynamicHandlerMessage<Link>>,
    reply_rx: mpsc::UnboundedReceiver<ReplyMessage>,
    mut handler: impl JrMessageHandler<Link = Link>,
) -> Result<(), crate::Error> {
    let mut my_rx = transport_rx
        .map(IncomingProtocolMsg::Transport)
        .merge(dynamic_handler_rx.map(IncomingProtocolMsg::DynamicHandler))
        .merge(reply_rx.map(IncomingProtocolMsg::Reply));

    let mut dynamic_handlers: FxHashMap<Uuid, Box<dyn DynamicHandler<Link>>> = FxHashMap::default();
    let mut pending_messages: Vec<MessageCx> = vec![];
    let mut state = <Link::State>::default();

    // Map from request ID to (method, sender) for response dispatch.
    // Keys are JSON values because jsonrpcmsg::Id doesn't implement Eq.
    // The method is stored to allow routing responses through typed handlers.
    let mut pending_replies: HashMap<
        serde_json::Value,
        (String, oneshot::Sender<crate::jsonrpc::ResponsePayload>),
    > = HashMap::new();

    while let Some(message_result) = my_rx.next().await {
        tracing::trace!(message = ?message_result, actor = "incoming_protocol_actor");
        match message_result {
            IncomingProtocolMsg::Reply(message) => match message {
                ReplyMessage::Subscribe { id, method, sender } => {
                    tracing::trace!(?id, %method, "incoming_actor: subscribing to response");
                    let id = serde_json::to_value(&id).unwrap();
                    pending_replies.insert(id, (method, sender));
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
                            .dyn_handle_message(pending_message, json_rpc_cx.clone())
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
                        dispatch_request(
                            json_rpc_cx,
                            request,
                            &mut dynamic_handlers,
                            &mut handler,
                            &mut pending_messages,
                            &mut state,
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
                            if let Some((method, sender)) = pending_replies.remove(&id_json) {
                                // Route the response through the handler chain
                                dispatch_response(
                                    json_rpc_cx,
                                    id,
                                    method,
                                    result,
                                    sender,
                                    &mut dynamic_handlers,
                                    &mut handler,
                                    &mut state,
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
                    json_rpc_cx.send_error_notification(error)?;
                }
            },
        }
    }
    Ok(())
}

#[derive(Debug)]
enum IncomingProtocolMsg<Link: JrLink> {
    Transport(Result<jsonrpcmsg::Message, crate::Error>),
    DynamicHandler(DynamicHandlerMessage<Link>),
    Reply(ReplyMessage),
}

/// Dispatches a JSON-RPC request to the handler.
/// Report an error back to the server if it does not get handled.
async fn dispatch_request<Link: JrLink>(
    json_rpc_cx: &JrConnectionCx<Link>,
    request: jsonrpcmsg::Request,
    dynamic_handlers: &mut FxHashMap<Uuid, Box<dyn DynamicHandler<Link>>>,
    handler: &mut impl JrMessageHandler<Link = Link>,
    pending_messages: &mut Vec<MessageCx>,
    state: &mut Link::State,
) -> Result<(), crate::Error> {
    let message = UntypedMessage::new(&request.method, &request.params).expect("well-formed JSON");

    let mut retry_any = false;
    let mut message_cx = match &request.id {
        Some(id) => MessageCx::Request(
            message,
            JrRequestCx::new(
                json_rpc_cx.message_tx.clone(),
                request.method.clone(),
                id.clone(),
            ),
        ),
        None => MessageCx::Notification(message),
    };

    // First, apply the handlers given by the user.
    match handler
        .handle_message(message_cx, json_rpc_cx.clone())
        .await?
    {
        Handled::Yes => {
            tracing::trace!(method = request.method, ?request.id, handler = ?handler.describe_chain(), "Handled successfully");
            return Ok(());
        }

        Handled::No { message: m, retry } => {
            tracing::trace!(method = ?request.method, ?request.id, handler = ?handler.describe_chain(), "Handler declined message");
            message_cx = m;
            retry_any |= retry;
        }
    }

    // Next, apply any dynamic handlers.
    for dynamic_handler in dynamic_handlers.values_mut() {
        match dynamic_handler
            .dyn_handle_message(message_cx, json_rpc_cx.clone())
            .await?
        {
            Handled::Yes => {
                tracing::trace!(method = request.method, ?request.id, handler = ?dynamic_handler.dyn_describe_chain(), "Message handled");
                return Ok(());
            }

            Handled::No { message: m, retry } => {
                tracing::trace!(method = ?request.method, ?request.id, handler = ?dynamic_handler.dyn_describe_chain(),  "Handler declined message");
                retry_any |= retry;
                message_cx = m;
            }
        }
    }

    // Finally, apply the default handler for the role.
    match Link::default_message_handler(message_cx, json_rpc_cx.clone(), state).await? {
        Handled::Yes => {
            tracing::trace!(method = ?request.method, handler = "default", "Handled successfully");
            return Ok(());
        }
        Handled::No { message: m, retry } => {
            tracing::trace!(method = ?request.method, handler = "default", "Not handled");
            message_cx = m;
            retry_any |= retry;
        }
    }

    // If the message was never handled, check whether the retry flag was set.
    // If so, enqueue it for later processing. Else, reject it.
    if retry_any {
        tracing::debug!(method = ?request.method, "Retrying message as new dynamic handlers are added");
        pending_messages.push(message_cx);
        Ok(())
    } else {
        tracing::debug!(method = ?request.method, "Rejecting message");
        let method = message_cx.method().to_string();
        message_cx.respond_with_error(
            crate::Error::method_not_found().data(method),
            json_rpc_cx.clone(),
        )
    }
}

/// Dispatches a JSON-RPC response through the handler chain.
///
/// This allows handlers to intercept and process responses before they reach
/// the awaiting code. The default behavior is to forward the response to the
/// local awaiter via the oneshot channel.
async fn dispatch_response<Link: JrLink>(
    json_rpc_cx: &JrConnectionCx<Link>,
    id: jsonrpcmsg::Id,
    method: String,
    result: Result<serde_json::Value, crate::Error>,
    sender: oneshot::Sender<crate::jsonrpc::ResponsePayload>,
    dynamic_handlers: &mut FxHashMap<Uuid, Box<dyn DynamicHandler<Link>>>,
    handler: &mut impl JrMessageHandler<Link = Link>,
    state: &mut Link::State,
) -> Result<(), crate::Error> {
    // Create a MessageCx::Response with a JrResponseCx that routes to the oneshot
    let response_cx = JrResponseCx::new(method.clone(), id.clone(), sender);
    let mut message_cx = MessageCx::Response(result, response_cx);

    // First, apply the handlers given by the user.
    match handler
        .handle_message(message_cx, json_rpc_cx.clone())
        .await?
    {
        Handled::Yes => {
            tracing::trace!(%method, ?id, handler = ?handler.describe_chain(), "Response handled");
            return Ok(());
        }

        Handled::No {
            message: m,
            retry: _,
        } => {
            tracing::trace!(%method, ?id, handler = ?handler.describe_chain(), "Handler declined response");
            message_cx = m;
        }
    }

    // Next, apply any dynamic handlers.
    for dynamic_handler in dynamic_handlers.values_mut() {
        match dynamic_handler
            .dyn_handle_message(message_cx, json_rpc_cx.clone())
            .await?
        {
            Handled::Yes => {
                tracing::trace!(%method, ?id, handler = ?dynamic_handler.dyn_describe_chain(), "Response handled");
                return Ok(());
            }

            Handled::No {
                message: m,
                retry: _,
            } => {
                tracing::trace!(%method, ?id, handler = ?dynamic_handler.dyn_describe_chain(), "Handler declined response");
                message_cx = m;
            }
        }
    }

    // Finally, apply the default handler for the role.
    match Link::default_message_handler(message_cx, json_rpc_cx.clone(), state).await? {
        Handled::Yes => {
            tracing::trace!(%method, ?id, handler = "default", "Response handled");
            return Ok(());
        }
        Handled::No {
            message: m,
            retry: _,
        } => {
            message_cx = m;
        }
    }

    // If no handler processed the response, forward it to the awaiter.
    // This is the default behavior - responses should reach their intended recipient.
    match message_cx {
        MessageCx::Response(result, request_cx) => {
            tracing::trace!(%method, ?id, "Forwarding response to awaiter");
            request_cx.respond_with_result(result)
        }
        _ => {
            // This shouldn't happen - handlers shouldn't transform Response into something else
            tracing::warn!(%method, ?id, "Response was transformed into non-response message");
            Ok(())
        }
    }
}
