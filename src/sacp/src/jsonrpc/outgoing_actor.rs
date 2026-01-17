// Types re-exported from crate root
use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::channel::oneshot;
use uuid::Uuid;

use crate::jsonrpc::OutgoingMessage;
use crate::jsonrpc::ReplyMessage;

/// Channel sender for outgoing JSON-RPC messages.
///
/// This is the primary interface for sending messages to the transport layer.
pub type OutgoingMessageTx = mpsc::UnboundedSender<OutgoingMessage>;

/// Send a raw message through the outgoing message channel.
///
/// This is a low-level API typically only used internally by the framework.
pub(crate) fn send_raw_message(
    tx: &OutgoingMessageTx,
    message: OutgoingMessage,
) -> Result<(), crate::Error> {
    tracing::debug!(?message, ?tx, "send_raw_message");
    tx.unbounded_send(message)
        .map_err(crate::util::internal_error)
}

/// Messages sent to the transport layer.
///
/// This enum wraps both regular JSON-RPC messages and control messages like flush.
#[derive(Debug)]
pub enum TransportMessage {
    /// Regular JSON-RPC message to be sent over the transport.
    Data(Result<jsonrpcmsg::Message, crate::Error>),

    /// Flush command: ensures all previous Data messages have been sent.
    /// The provided oneshot sender is signaled when the flush is complete.
    Flush(oneshot::Sender<()>),
}

/// Outgoing protocol actor: Converts application-level OutgoingMessage to protocol-level jsonrpcmsg::Message.
///
/// This actor handles JSON-RPC protocol semantics:
/// - Assigns unique IDs to outgoing requests
/// - Subscribes to reply_actor for response correlation
/// - Converts OutgoingMessage variants to jsonrpcmsg::Message
///
/// This is the protocol layer - it has no knowledge of how messages are transported.
pub(super) async fn outgoing_protocol_actor(
    mut outgoing_rx: mpsc::UnboundedReceiver<OutgoingMessage>,
    reply_tx: mpsc::UnboundedSender<ReplyMessage>,
    transport_tx: mpsc::UnboundedSender<TransportMessage>,
) -> Result<(), crate::Error> {
    while let Some(message) = outgoing_rx.next().await {
        tracing::debug!(?message, "outgoing_protocol_actor");

        // Create the message to be sent over the transport
        let transport_message = match message {
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

                TransportMessage::Data(Ok(jsonrpcmsg::Message::Request(
                    jsonrpcmsg::Request::new_v2(method, params, Some(id)),
                )))
            }
            OutgoingMessage::Notification { method, params } => TransportMessage::Data(Ok(
                jsonrpcmsg::Message::Request(jsonrpcmsg::Request::new_v2(method, params, None)),
            )),
            OutgoingMessage::Response {
                id,
                response: Ok(value),
            } => {
                tracing::debug!(?id, "Sending success response");
                TransportMessage::Data(Ok(jsonrpcmsg::Message::Response(
                    jsonrpcmsg::Response::success_v2(value, Some(id)),
                )))
            }
            OutgoingMessage::Response {
                id,
                response: Err(error),
            } => {
                tracing::warn!(?id, ?error, "Sending error response");
                // Convert crate::Error to jsonrpcmsg::Error
                let jsonrpc_error = jsonrpcmsg::Error {
                    code: error.code.into(),
                    message: error.message,
                    data: error.data,
                };
                TransportMessage::Data(Ok(jsonrpcmsg::Message::Response(
                    jsonrpcmsg::Response::error_v2(jsonrpc_error, Some(id)),
                )))
            }
            OutgoingMessage::Error { error } => {
                // Convert crate::Error to jsonrpcmsg::Error
                let jsonrpc_error = jsonrpcmsg::Error {
                    code: error.code.into(),
                    message: error.message,
                    data: error.data,
                };
                // Response with id: None means this is an error notification that couldn't be
                // correlated to a specific request (e.g., parse error before we could read the id)
                TransportMessage::Data(Ok(jsonrpcmsg::Message::Response(
                    jsonrpcmsg::Response::error_v2(jsonrpc_error, None),
                )))
            }
            OutgoingMessage::Flush { responder } => {
                // Forward flush to transport layer for reliable synchronization
                TransportMessage::Flush(responder)
            }
        };

        // Send to transport layer
        transport_tx
            .unbounded_send(transport_message)
            .map_err(crate::Error::into_internal_error)?;
    }
    Ok(())
}
