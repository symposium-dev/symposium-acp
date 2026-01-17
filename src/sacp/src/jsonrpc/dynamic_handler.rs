use futures::future::BoxFuture;
use uuid::Uuid;

use crate::link::JrLink;
use crate::{ConnectionTo, HandleMessageFrom, Handled, MessageCx};

/// Internal dyn-safe wrapper around `HandleMessageFrom`
pub(crate) trait DynamicHandler<Link>: Send {
    fn dyn_handle_message(
        &mut self,
        message: MessageCx,
        cx: ConnectionTo<Link>,
    ) -> BoxFuture<'_, Result<Handled<MessageCx>, crate::Error>>;

    fn dyn_describe_chain(&self) -> String;
}

impl<H: HandleMessageFrom> DynamicHandler<H::Link> for H {
    fn dyn_handle_message(
        &mut self,
        message: MessageCx,
        cx: ConnectionTo<H::Link>,
    ) -> BoxFuture<'_, Result<Handled<MessageCx>, crate::Error>> {
        Box::pin(HandleMessageFrom::handle_message(self, message, cx))
    }

    fn dyn_describe_chain(&self) -> String {
        format!("{:?}", H::describe_chain(self))
    }
}

/// Messages used to add/remove dynamic handlers
pub(crate) enum DynamicHandlerMessage<Link> {
    AddDynamicHandler(Uuid, Box<dyn DynamicHandler<Link>>),
    RemoveDynamicHandler(Uuid),
}

impl<Link: JrLink> std::fmt::Debug for DynamicHandlerMessage<Link> {
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
