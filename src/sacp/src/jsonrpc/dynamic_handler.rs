use futures::future::BoxFuture;
use uuid::Uuid;

use crate::role::JrRole;
use crate::{Handled, JrConnectionCx, MessageCx, jsonrpc::JrMessageHandlerSend};

/// Internal dyn-safe wrapper around `JrMessageHandlerSend`
pub(crate) trait DynamicHandler<Role>: Send {
    fn dyn_handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<Role>,
    ) -> BoxFuture<'_, Result<Handled<MessageCx>, crate::Error>>;

    fn dyn_describe_chain(&self) -> String;
}

impl<H: JrMessageHandlerSend> DynamicHandler<H::Role> for H {
    fn dyn_handle_message(
        &mut self,
        message: MessageCx,
        cx: JrConnectionCx<H::Role>,
    ) -> BoxFuture<'_, Result<Handled<MessageCx>, crate::Error>> {
        Box::pin(JrMessageHandlerSend::handle_message(self, message, cx))
    }

    fn dyn_describe_chain(&self) -> String {
        format!("{:?}", H::describe_chain(self))
    }
}

/// Messages used to add/remove dynamic handlers
pub(crate) enum DynamicHandlerMessage<Role> {
    AddDynamicHandler(Uuid, Box<dyn DynamicHandler<Role>>),
    RemoveDynamicHandler(Uuid),
}

impl<Role: JrRole> std::fmt::Debug for DynamicHandlerMessage<Role> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddDynamicHandler(arg0, arg1) => f
                .debug_tuple("AddDynamicHandler")
                .field(arg0)
                .field(&arg1.dyn_describe_chain())
                .finish(),
            Self::RemoveDynamicHandler(arg0) => {
                f.debug_tuple("RemoveDynamicHandler").field(arg0).finish()
            }
        }
    }
}
