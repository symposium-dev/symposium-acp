use futures::future::BoxFuture;
use uuid::Uuid;

use crate::role::Role;
use crate::{ConnectionTo, HandleMessageFrom, Handled, Dispatch};

/// Internal dyn-safe wrapper around `HandleMessageAs`
///
/// The type parameter `R` is the role's counterpart (who we connect to).
pub(crate) trait DynHandleMessageFrom<Counterpart: Role>: Send {
    fn dyn_handle_message_from(
        &mut self,
        message: Dispatch,
        cx: ConnectionTo<Counterpart>,
    ) -> BoxFuture<'_, Result<Handled<Dispatch>, crate::Error>>;

    fn dyn_describe_chain(&self) -> String;
}

impl<Counterpart: Role, H: HandleMessageFrom<Counterpart>> DynHandleMessageFrom<Counterpart> for H {
    fn dyn_handle_message_from(
        &mut self,
        message: Dispatch,
        cx: ConnectionTo<Counterpart>,
    ) -> BoxFuture<'_, Result<Handled<Dispatch>, crate::Error>> {
        Box::pin(HandleMessageFrom::handle_message_from(self, message, cx))
    }

    fn dyn_describe_chain(&self) -> String {
        format!("{:?}", H::describe_chain(self))
    }
}

/// Messages used to add/remove dynamic handlers
pub(crate) enum DynamicHandlerMessage<Counterpart: Role> {
    AddDynamicHandler(Uuid, Box<dyn DynHandleMessageFrom<Counterpart>>),
    RemoveDynamicHandler(Uuid),
}

impl<Counterpart: Role> std::fmt::Debug for DynamicHandlerMessage<Counterpart> {
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
