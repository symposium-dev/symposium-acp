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
use crate::role::JrRole;

use super::Handled;

/// Incoming protocol actor: Routes jsonrpcmsg::Message to reply_actor or handler.
///
/// This actor handles JSON-RPC protocol semantics:
/// - Routes responses to reply_actor (for request/response correlation)
/// - Routes requests/notifications to handler chain
/// - Converts jsonrpcmsg::Request to UntypedMessage for handlers
///
/// This is the protocol layer - it has no knowledge of how messages arrived.
pub(super) async fn incoming_protocol_actor<Role: JrRole>(
    json_rpc_cx: &JrConnectionCx<Role>,
    transport_rx: mpsc::UnboundedReceiver<Result<jsonrpcmsg::Message, crate::Error>>,
    dynamic_handler_rx: mpsc::UnboundedReceiver<DynamicHandlerMessage<Role>>,
    reply_tx: mpsc::UnboundedSender<ReplyMessage>,
    mut handler: impl JrMessageHandler<Role = Role>,
) -> Result<(), crate::Error> {
    let mut my_rx = transport_rx
        .map(IncomingProtocolMsg::Transport)
        .merge(dynamic_handler_rx.map(IncomingProtocolMsg::DynamicHandler));

    let mut dynamic_handlers: FxHashMap<Uuid, Box<dyn DynamicHandler<Role>>> = FxHashMap::default();

    while let Some(message_result) = my_rx.next().await {
        match message_result {
            IncomingProtocolMsg::DynamicHandler(message) => match message {
                DynamicHandlerMessage::AddDynamicHandler(uuid, handler) => {
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
                        dispatch_request(json_rpc_cx, request, &mut dynamic_handlers, &mut handler)
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

enum IncomingProtocolMsg<Role: JrRole> {
    Transport(Result<jsonrpcmsg::Message, crate::Error>),
    DynamicHandler(DynamicHandlerMessage<Role>),
}

/// Dispatches a JSON-RPC request to the handler.
/// Report an error back to the server if it does not get handled.
async fn dispatch_request<Role: JrRole>(
    json_rpc_cx: &JrConnectionCx<Role>,
    request: jsonrpcmsg::Request,
    dynamic_handlers: &mut FxHashMap<Uuid, Box<dyn DynamicHandler<Role>>>,
    handler: &mut impl JrMessageHandler<Role = Role>,
) -> Result<(), crate::Error> {
    let message = UntypedMessage::new(&request.method, &request.params).expect("well-formed JSON");

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

    for dynamic_handler in dynamic_handlers.values_mut() {
        match dynamic_handler
            .dyn_handle_message(message_cx, json_rpc_cx.clone())
            .await?
        {
            Handled::Yes => {
                tracing::debug!(method = request.method, ?request.id, handler = ?dynamic_handler.dyn_describe_chain(), "Message handled");
                return Ok(());
            }

            Handled::No(m) => {
                tracing::debug!(method = ?request.method, ?request.id, handler = ?dynamic_handler.dyn_describe_chain(),  "Dynamic handler declined message");
                message_cx = m;
            }
        }
    }

    match handler
        .handle_message(message_cx, json_rpc_cx.clone())
        .await?
    {
        Handled::Yes => {
            tracing::debug!(method = request.method, ?request.id, handler = ?handler.describe_chain(), "Message handled");
            return Ok(());
        }

        Handled::No(m) => {
            tracing::debug!(method = ?request.method, ?request.id, "No suitable handler found");
            Role::default_message_handler(m, json_rpc_cx.clone()).await
        }
    }
}
