use crate::jsonrpc::{Handled, JsonRpcHandler};
use crate::{JsonRpcConnectionCx, JsonRpcNotification, JsonRpcRequest, MessageAndCx};
use agent_client_protocol as acp;
use std::marker::PhantomData;
use std::ops::AsyncFnMut;

use super::JsonRpcRequestCx;

/// Null handler that accepts no messages.
#[derive(Default)]
pub struct NullHandler {}

impl JsonRpcHandler for NullHandler {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "(null)"
    }

    async fn handle_message(
        &mut self,
        message: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, agent_client_protocol::Error> {
        Ok(Handled::No(message))
    }
}

pub struct RequestHandler<R, F>
where
    R: JsonRpcRequest,
    F: AsyncFnMut(R, JsonRpcRequestCx<R::Response>) -> Result<(), acp::Error>,
{
    handler: F,
    phantom: PhantomData<fn(R)>,
}

impl<R, F> RequestHandler<R, F>
where
    R: JsonRpcRequest,
    F: AsyncFnMut(R, JsonRpcRequestCx<R::Response>) -> Result<(), acp::Error>,
{
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            phantom: PhantomData,
        }
    }
}

impl<R, F> JsonRpcHandler for RequestHandler<R, F>
where
    R: JsonRpcRequest,
    F: AsyncFnMut(R, JsonRpcRequestCx<R::Response>) -> Result<(), acp::Error>,
{
    async fn handle_message(
        &mut self,
        message_cx: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, agent_client_protocol::Error> {
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

pub struct NotificationHandler<N, F>
where
    N: JsonRpcNotification,
    F: AsyncFnMut(N, JsonRpcConnectionCx) -> Result<(), acp::Error>,
{
    handler: F,
    phantom: PhantomData<fn(N)>,
}

impl<R, F> NotificationHandler<R, F>
where
    R: JsonRpcNotification,
    F: AsyncFnMut(R, JsonRpcConnectionCx) -> Result<(), acp::Error>,
{
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            phantom: PhantomData,
        }
    }
}

impl<R, F> JsonRpcHandler for NotificationHandler<R, F>
where
    R: JsonRpcNotification,
    F: AsyncFnMut(R, JsonRpcConnectionCx) -> Result<(), acp::Error>,
{
    async fn handle_message(
        &mut self,
        message_cx: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, agent_client_protocol::Error> {
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
    R: JsonRpcRequest,
    N: JsonRpcNotification,
    F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), acp::Error>,
{
    handler: F,
    phantom: PhantomData<fn(R, N)>,
}

impl<R, N, F> MessageHandler<R, N, F>
where
    R: JsonRpcRequest,
    N: JsonRpcNotification,
    F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), acp::Error>,
{
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            phantom: PhantomData,
        }
    }
}

impl<R, N, F> JsonRpcHandler for MessageHandler<R, N, F>
where
    R: JsonRpcRequest,
    N: JsonRpcNotification,
    F: AsyncFnMut(MessageAndCx<R, N>) -> Result<(), acp::Error>,
{
    async fn handle_message(
        &mut self,
        message_cx: MessageAndCx,
    ) -> Result<Handled<MessageAndCx>, agent_client_protocol::Error> {
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
        format!("({}, {})", std::any::type_name::<R>(), std::any::type_name::<N>())
    }
}

pub struct ChainHandler<H1, H2>
where
    H1: JsonRpcHandler,
    H2: JsonRpcHandler,
{
    handler1: H1,
    handler2: H2,
}

impl<H1, H2> ChainHandler<H1, H2>
where
    H1: JsonRpcHandler,
    H2: JsonRpcHandler,
{
    pub fn new(handler1: H1, handler2: H2) -> Self {
        Self { handler1, handler2 }
    }
}

impl<H1, H2> JsonRpcHandler for ChainHandler<H1, H2>
where
    H1: JsonRpcHandler,
    H2: JsonRpcHandler,
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

        impl<H1: JsonRpcHandler, H2: JsonRpcHandler> std::fmt::Debug for DebugImpl<'_, H1, H2> {
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
    ) -> Result<Handled<MessageAndCx>, agent_client_protocol::Error> {
        match self.handler1.handle_message(message).await? {
            Handled::Yes => Ok(Handled::Yes),
            Handled::No(message) => self.handler2.handle_message(message).await,
        }
    }
}
