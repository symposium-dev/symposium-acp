use crate::jsonrpc::Handled;
use crate::{JrConnectionCx, JrNotification, JrRequest, MessageAndCx};
// Types re-exported from crate root
use std::marker::PhantomData;
use std::ops::AsyncFnMut;

use super::JrRequestCx;

/// Handlers are invoked when new messages arrive at the [`JrConnection`].
/// They have a chance to inspect the method and parameters and decide whether to "claim" the request
/// (i.e., handle it). If they do not claim it, the request will be passed to the next handler.
#[allow(async_fn_in_trait)]
pub trait JrMessageHandler {
    /// Attempt to claim an incoming message (request or notification).
    ///
    /// # Important: do not block
    ///
    /// The server will not process new messages until this handler returns.
    /// You should avoid blocking in this callback unless you wish to block the server (e.g., for rate limiting).
    /// The recommended approach to manage expensive operations is to the [`JrConnectionCx::spawn`] method available on the message context.
    ///
    /// # Parameters
    ///
    /// * `cx` - The context of the request. This gives access to the request ID and the method name and is used to send a reply; can also be used to send other messages to the other party.
    /// * `params` - The parameters of the request.
    ///
    /// # Returns
    ///
    /// * `Ok(Handled::Yes)` if the message was claimed. It will not be propagated further.
    /// * `Ok(Handled::No(message))` if not; the (possibly changed) message will be passed to the remaining handlers.
    /// * `Err` if an internal error occurs (this will bring down the server).
    async fn handle_message(
        &mut self,
        message: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, crate::Error>;

    /// Returns a debug description of the handler chain for diagnostics
    fn describe_chain(&self) -> impl std::fmt::Debug;
}

/// Handlers are invoked when new messages arrive at the [`JrConnection`].
/// They have a chance to inspect the method and parameters and decide whether to "claim" the request
/// (i.e., handle it). If they do not claim it, the request will be passed to the next handler.
#[expect(missing_docs)]
pub trait IntoJrMessageHandler {
    type Handler: JrMessageHandler;

    fn into_jr_message_handler(self, cx: &JrConnectionCx) -> Result<Self::Handler, crate::Error>;
}

/// Null handler that accepts no messages.
#[derive(Default)]
pub struct NullHandler {}

impl JrMessageHandler for NullHandler {
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

impl IntoJrMessageHandler for NullHandler {
    type Handler = Self;

    fn into_jr_message_handler(self, _cx: &JrConnectionCx) -> Result<Self::Handler, crate::Error> {
        Ok(self)
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

impl<R, F> IntoJrMessageHandler for RequestHandler<R, F>
where
    R: JrRequest,
    F: AsyncFnMut(R, JrRequestCx<R::Response>) -> Result<(), crate::Error>,
{
    type Handler = Self;

    fn into_jr_message_handler(self, _cx: &JrConnectionCx) -> Result<Self::Handler, crate::Error> {
        Ok(self)
    }
}

impl<R, F> JrMessageHandler for RequestHandler<R, F>
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

impl<R, F> IntoJrMessageHandler for NotificationHandler<R, F>
where
    R: JrNotification,
    F: AsyncFnMut(R, JrConnectionCx) -> Result<(), crate::Error>,
{
    type Handler = Self;

    fn into_jr_message_handler(self, _cx: &JrConnectionCx) -> Result<Self::Handler, crate::Error> {
        Ok(self)
    }
}

impl<R, F> JrMessageHandler for NotificationHandler<R, F>
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

impl<R, N, F> IntoJrMessageHandler for MessageHandler<R, N, F>
where
    R: JrRequest,
    N: JrNotification,
    F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), crate::Error>,
{
    type Handler = Self;

    fn into_jr_message_handler(self, _cx: &JrConnectionCx) -> Result<Self::Handler, crate::Error> {
        Ok(self)
    }
}

impl<R, N, F> JrMessageHandler for MessageHandler<R, N, F>
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
pub struct ChainedHandler<H1, H2> {
    handler1: H1,
    handler2: H2,
}

impl<H1, H2> ChainedHandler<H1, H2> {
    /// Creates a new chain handler
    pub fn new(handler1: H1, handler2: H2) -> Self {
        Self { handler1, handler2 }
    }
}

impl<H1: IntoJrMessageHandler, H2: IntoJrMessageHandler> IntoJrMessageHandler
    for ChainedHandler<H1, H2>
{
    type Handler = ChainedHandler<H1::Handler, H2::Handler>;

    fn into_jr_message_handler(self, cx: &JrConnectionCx) -> Result<Self::Handler, crate::Error> {
        Ok(ChainedHandler {
            handler1: self.handler1.into_jr_message_handler(cx)?,
            handler2: self.handler2.into_jr_message_handler(cx)?,
        })
    }
}

impl<H1, H2> JrMessageHandler for ChainedHandler<H1, H2>
where
    H1: JrMessageHandler,
    H2: JrMessageHandler,
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

        impl<H1: JrMessageHandler, H2: JrMessageHandler> std::fmt::Debug for DebugImpl<'_, H1, H2> {
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
