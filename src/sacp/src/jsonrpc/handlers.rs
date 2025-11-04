use crate::jsonrpc::{Handled, JrHandler};
use crate::{JrConnectionCx, JrNotification, JrRequest, MessageAndCx};
// Types re-exported from crate root
use std::marker::PhantomData;
use std::ops::AsyncFnMut;

use super::JrRequestCx;

/// Null handler that accepts no messages.
#[derive(Default)]
pub struct NullHandler {}

impl JrHandler for NullHandler {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "(null)"
    }

    async fn handle_message(
        &mut self,
        message: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, crate::Error> {
        Ok(Handled::No(message))
    }
}

/// Handler for typed request messages
pub struct RequestHandler<R, F>
where
    R: JrRequest,
    F: AsyncFnMut(R, JrRequestCx<R::Response>) -> Result<(), crate::Error>,
{
    handler: F,
    phantom: PhantomData<fn(R)>,
}

impl<R, F> RequestHandler<R, F>
where
    R: JrRequest,
    F: AsyncFnMut(R, JrRequestCx<R::Response>) -> Result<(), crate::Error>,
{
    /// Creates a new request handler
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            phantom: PhantomData,
        }
    }
}

impl<R, F> JrHandler for RequestHandler<R, F>
where
    R: JrRequest,
    F: AsyncFnMut(R, JrRequestCx<R::Response>) -> Result<(), crate::Error>,
{
    async fn handle_message(
        &mut self,
        message_cx: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, crate::Error> {
        match message_cx {
            MessageAndCx::Request(message, request_cx) => {
                tracing::debug!(
                    request_type = std::any::type_name::<R>(),
                    message = ?message,
                    "RequestHandler::handle_request"
                );
                match R::parse_request(&message.method, &message.params) {
                    Some(Ok(req)) => {
                        tracing::trace!(?req, "RequestHandler::handle_request: parse completed");
                        (self.handler)(req, request_cx.cast()).await?;
                        Ok(Handled::Yes)
                    }
                    Some(Err(err)) => {
                        tracing::trace!(?err, "RequestHandler::handle_request: parse errored");
                        Err(err)
                    }
                    None => {
                        tracing::trace!("RequestHandler::handle_request: parse failed");
                        Ok(Handled::No(MessageAndCx::Request(message, request_cx)))
                    }
                }
            }

            MessageAndCx::Notification(..) => Ok(Handled::No(message_cx)),
        }
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        std::any::type_name::<R>()
    }
}

/// Handler for typed notification messages
pub struct NotificationHandler<N, F>
where
    N: JrNotification,
    F: AsyncFnMut(N, JrConnectionCx) -> Result<(), crate::Error>,
{
    handler: F,
    phantom: PhantomData<fn(N)>,
}

impl<R, F> NotificationHandler<R, F>
where
    R: JrNotification,
    F: AsyncFnMut(R, JrConnectionCx) -> Result<(), crate::Error>,
{
    /// Creates a new notification handler
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            phantom: PhantomData,
        }
    }
}

impl<R, F> JrHandler for NotificationHandler<R, F>
where
    R: JrNotification,
    F: AsyncFnMut(R, JrConnectionCx) -> Result<(), crate::Error>,
{
    async fn handle_message(
        &mut self,
        message_cx: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, crate::Error> {
        match message_cx {
            MessageAndCx::Notification(message, cx) => {
                tracing::debug!(
                    request_type = std::any::type_name::<R>(),
                    message = ?message,
                    "NotificationHandler::handle_message"
                );
                match R::parse_notification(&message.method, &message.params) {
                    Some(Ok(req)) => {
                        tracing::trace!(
                            ?req,
                            "NotificationHandler::handle_notification: parse completed"
                        );
                        (self.handler)(req, cx).await?;
                        Ok(Handled::Yes)
                    }
                    Some(Err(err)) => {
                        tracing::trace!(?err, "RequestHandler::handle_request: parse errored");
                        Err(err)
                    }
                    None => {
                        tracing::trace!("RequestHandler::handle_request: parse failed");
                        Ok(Handled::No(MessageAndCx::Notification(message, cx)))
                    }
                }
            }

            MessageAndCx::Request(..) => Ok(Handled::No(message_cx)),
        }
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        std::any::type_name::<R>()
    }
}

/// Handler that tries H1 and then H2.

pub struct MessageHandler<R, N, F>
where
    R: JrRequest,
    N: JrNotification,
    F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), crate::Error>,
{
    handler: F,
    phantom: PhantomData<fn(R, N)>,
}

impl<R, N, F> MessageHandler<R, N, F>
where
    R: JrRequest,
    N: JrNotification,
    F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), crate::Error>,
{
    /// Creates a new message handler
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            phantom: PhantomData,
        }
    }
}

impl<R, N, F> JrHandler for MessageHandler<R, N, F>
where
    R: JrRequest,
    N: JrNotification,
    F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), crate::Error>,
{
    async fn handle_message(
        &mut self,
        message_cx: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, crate::Error> {
        match message_cx {
            MessageAndCx::Request(message, request_cx) => {
                tracing::debug!(
                    request_type = std::any::type_name::<R>(),
                    message = ?message,
                    "MessageHandler::handle_request"
                );
                match R::parse_request(&message.method, &message.params) {
                    Some(Ok(req)) => {
                        tracing::trace!(?req, "MessageHandler::handle_request: parse completed");
                        let typed_message = MessageAndCx::Request(req, request_cx.cast());
                        (self.handler)(typed_message).await?;
                        Ok(Handled::Yes)
                    }
                    Some(Err(err)) => {
                        tracing::trace!(?err, "MessageHandler::handle_request: parse errored");
                        Err(err)
                    }
                    None => {
                        tracing::trace!("MessageHandler::handle_request: parse failed");
                        Ok(Handled::No(MessageAndCx::Request(message, request_cx)))
                    }
                }
            }

            MessageAndCx::Notification(message, cx) => {
                tracing::debug!(
                    notification_type = std::any::type_name::<N>(),
                    message = ?message,
                    "MessageHandler::handle_notification"
                );
                match N::parse_notification(&message.method, &message.params) {
                    Some(Ok(notif)) => {
                        tracing::trace!(
                            ?notif,
                            "MessageHandler::handle_notification: parse completed"
                        );
                        let typed_message = MessageAndCx::Notification(notif, cx);
                        (self.handler)(typed_message).await?;
                        Ok(Handled::Yes)
                    }
                    Some(Err(err)) => {
                        tracing::trace!(?err, "MessageHandler::handle_notification: parse errored");
                        Err(err)
                    }
                    None => {
                        tracing::trace!("MessageHandler::handle_notification: parse failed");
                        Ok(Handled::No(MessageAndCx::Notification(message, cx)))
                    }
                }
            }
        }
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!(
            "({}, {})",
            std::any::type_name::<R>(),
            std::any::type_name::<N>()
        )
    }
}

/// Chains two handlers together, trying the first handler and falling back to the second
pub struct ChainHandler<H1, H2>
where
    H1: JrHandler,
    H2: JrHandler,
{
    handler1: H1,
    handler2: H2,
}

impl<H1, H2> ChainHandler<H1, H2>
where
    H1: JrHandler,
    H2: JrHandler,
{
    /// Creates a new chain handler
    pub fn new(handler1: H1, handler2: H2) -> Self {
        Self { handler1, handler2 }
    }
}

impl<H1, H2> JrHandler for ChainHandler<H1, H2>
where
    H1: JrHandler,
    H2: JrHandler,
{
    fn describe_chain(&self) -> impl std::fmt::Debug {
        return DebugImpl {
            handler1: &self.handler1,
            handler2: &self.handler2,
        };

        struct DebugImpl<'h, H1, H2> {
            handler1: &'h H1,
            handler2: &'h H2,
        }

        impl<H1: JrHandler, H2: JrHandler> std::fmt::Debug for DebugImpl<'_, H1, H2> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "{:?}, {:?}",
                    self.handler1.describe_chain(),
                    self.handler2.describe_chain()
                )
            }
        }
    }

    async fn handle_message(
        &mut self,
        message: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, crate::Error> {
        match self.handler1.handle_message(message).await? {
            Handled::Yes => Ok(Handled::Yes),
            Handled::No(message) => self.handler2.handle_message(message).await,
        }
    }
}
