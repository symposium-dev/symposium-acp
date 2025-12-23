// Types re-exported from crate root
use futures::StreamExt as _;
use futures::channel::mpsc;
use futures_concurrency::stream::StreamExt as _;
use fxhash::FxHashMap;
use uuid::Uuid;

use crate::MessageCx;
use crate::UntypedMessage;
use crate::jsonrpc::JrConnectionCx;
use crate::jsonrpc::JrMessageHandler;
use crate::jsonrpc::JrRequestCx;
use crate::jsonrpc::ReplyMessage;
use crate::jsonrpc::dynamic_handler::DynamicHandler;
use crate::jsonrpc::dynamic_handler::DynamicHandlerMessage;
use crate::role::JrLink;

use super::Handled;

/// Incoming protocol actor: Routes jsonrpcmsg::Message to reply_actor or handler.
///
/// This actor handles JSON-RPC protocol semantics:
/// - Routes responses to reply_actor (for request/response correlation)
/// - Routes requests/notifications to registered handlers
/// - Converts jsonrpcmsg::Request to UntypedMessage for handlers
///
/// This is the protocol layer - it has no knowledge of how messages arrived.
pub(super) async fn incoming_protocol_actor<Role: JrLink>(
    json_rpc_cx: &JrConnectionCx<Role>,
    transport_rx: mpsc::UnboundedReceiver<Result<jsonrpcmsg::Message, crate::Error>>,
    dynamic_handler_rx: mpsc::UnboundedReceiver<DynamicHandlerMessage<Role>>,
    reply_tx: mpsc::UnboundedSender<ReplyMessage>,
    mut handler: impl JrMessageHandler<Link = Role>,
) -> Result<(), crate::Error> {
    let mut my_rx = transport_rx
        .map(IncomingProtocolMsg::Transport)
        .merge(dynamic_handler_rx.map(IncomingProtocolMsg::DynamicHandler));

    let mut dynamic_handlers: FxHashMap<Uuid, Box<dyn DynamicHandler<Role>>> = FxHashMap::default();
    let mut pending_messages: Vec<MessageCx> = vec![];
    let mut state = <Role::State>::default();

    while let Some(message_result) = my_rx.next().await {
        tracing::trace!(message = ?message_result, actor = "incoming_protocol_actor");
        match message_result {
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
enum IncomingProtocolMsg<Role: JrLink> {
    Transport(Result<jsonrpcmsg::Message, crate::Error>),
    DynamicHandler(DynamicHandlerMessage<Role>),
}

/// Dispatches a JSON-RPC request to the handler.
/// Report an error back to the server if it does not get handled.
async fn dispatch_request<Role: JrLink>(
    json_rpc_cx: &JrConnectionCx<Role>,
    request: jsonrpcmsg::Request,
    dynamic_handlers: &mut FxHashMap<Uuid, Box<dyn DynamicHandler<Role>>>,
    handler: &mut impl JrMessageHandler<Link = Role>,
    pending_messages: &mut Vec<MessageCx>,
    state: &mut Role::State,
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
    match Role::default_message_handler(message_cx, json_rpc_cx.clone(), state).await? {
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
            crate::Error::method_not_found().with_data(method),
            json_rpc_cx.clone(),
        )
    }
}
