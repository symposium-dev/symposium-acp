use std::collections::HashMap;

// Types re-exported from crate root
use futures::StreamExt as _;
use futures::channel::mpsc;

use crate::jsonrpc::ReplyMessage;

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
