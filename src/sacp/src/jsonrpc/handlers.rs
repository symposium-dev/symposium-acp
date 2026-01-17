use crate::jsonrpc::{Handled, IntoHandled, JrMessageHandler, JsonRpcResponse};
use crate::link::{HasPeer, JrLink, handle_incoming_message};
use crate::peer::JrPeer;
use crate::{JrConnectionCx, JsonRpcNotification, JsonRpcRequest, MessageCx, UntypedMessage};
// Types re-exported from crate root
use super::JrRequestCx;
use std::marker::PhantomData;
use std::ops::AsyncFnMut;

/// Null handler that accepts no messages.
pub struct NullHandler<Link: JrLink> {
    role: Link,
}

impl<Link: JrLink> NullHandler<Link> {
    /// Creates a new null handler.
    pub fn new(role: Link) -> Self {
        Self { role }
    }

    /// Returns the role.
    pub fn role(&self) -> Link {
        self.role
    }
}

impl<Link: JrLink> JrMessageHandler for NullHandler<Link> {
    type Link = Link;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "(null)"
    }

    async fn handle_message(
        &mut self,
        message: MessageCx,
        _cx: JrConnectionCx<Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        Ok(Handled::No {
            message,
            retry: false,
        })
    }
}

/// Handler for typed request messages
pub struct RequestHandler<
    Link: JrLink,
    Peer: JrPeer,
    Req: JsonRpcRequest = UntypedMessage,
    F = (),
    ToFut = (),
> {
    handler: F,
    to_future_hack: ToFut,
    phantom: PhantomData<fn(Link, Peer, Req)>,
}

impl<Link: JrLink, Peer: JrPeer, Req: JsonRpcRequest, F, ToFut>
    RequestHandler<Link, Peer, Req, F, ToFut>
{
    /// Creates a new request handler
    pub fn new(_peer: Peer, _link: Link, handler: F, to_future_hack: ToFut) -> Self {
        Self {
            handler,
            to_future_hack,
            phantom: PhantomData,
        }
    }
}

impl<Link: JrLink, Peer: JrPeer, Req, F, T, ToFut> JrMessageHandler
    for RequestHandler<Link, Peer, Req, F, ToFut>
where
    Link: HasPeer<Peer>,
    Req: JsonRpcRequest,
    F: AsyncFnMut(Req, JrRequestCx<Req::Response>, JrConnectionCx<Link>) -> Result<T, crate::Error>
        + Send,
    T: crate::IntoHandled<(Req, JrRequestCx<Req::Response>)>,
    ToFut: Fn(
            &mut F,
            Req,
            JrRequestCx<Req::Response>,
            JrConnectionCx<Link>,
        ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
        + Send
        + Sync,
{
    type Link = Link;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        std::any::type_name::<Req>()
    }

    async fn handle_message(
        &mut self,
        message_cx: MessageCx,
        connection_cx: JrConnectionCx<Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        handle_incoming_message::<Link, Peer>(
            Peer::default(),
            message_cx,
            connection_cx,
            async |message_cx, connection_cx| {
                match message_cx {
                    MessageCx::Request(message, request_cx) => {
                        tracing::debug!(
                            request_type = std::any::type_name::<Req>(),
                            message = ?message,
                            "RequestHandler::handle_request"
                        );
                        if !Req::matches_method(&message.method) {
                            tracing::trace!("RequestHandler::handle_request: method doesn't match");
                            Ok(Handled::No {
                                message: MessageCx::Request(message, request_cx),
                                retry: false,
                            })
                        } else {
                            match Req::parse_message(&message.method, &message.params) {
                                Ok(req) => {
                                    tracing::trace!(
                                        ?req,
                                        "RequestHandler::handle_request: parse completed"
                                    );
                                    let typed_request_cx = request_cx.cast();
                                    let result = (self.to_future_hack)(
                                        &mut self.handler,
                                        req,
                                        typed_request_cx,
                                        connection_cx,
                                    )
                                    .await?;
                                    match result.into_handled() {
                                        Handled::Yes => Ok(Handled::Yes),
                                        Handled::No {
                                            message: (request, request_cx),
                                            retry,
                                        } => {
                                            // Handler returned the request back, convert to untyped
                                            let untyped = request.to_untyped_message()?;
                                            Ok(Handled::No {
                                                message: MessageCx::Request(
                                                    untyped,
                                                    request_cx.erase_to_json(),
                                                ),
                                                retry,
                                            })
                                        }
                                    }
                                }
                                Err(err) => {
                                    tracing::trace!(
                                        ?err,
                                        "RequestHandler::handle_request: parse errored"
                                    );
                                    Err(err)
                                }
                            }
                        }
                    }

                    MessageCx::Notification(..) | MessageCx::Response(..) => Ok(Handled::No {
                        message: message_cx,
                        retry: false,
                    }),
                }
            },
        )
        .await
    }
}

/// Handler for typed notification messages
pub struct NotificationHandler<
    Link: JrLink,
    Peer: JrPeer,
    Notif: JsonRpcNotification = UntypedMessage,
    F = (),
    ToFut = (),
> {
    handler: F,
    to_future_hack: ToFut,
    phantom: PhantomData<fn(Link, Peer, Notif)>,
}

impl<Link: JrLink, Peer: JrPeer, Notif: JsonRpcNotification, F, ToFut>
    NotificationHandler<Link, Peer, Notif, F, ToFut>
{
    /// Creates a new notification handler
    pub fn new(_peer: Peer, _link: Link, handler: F, to_future_hack: ToFut) -> Self {
        Self {
            handler,
            to_future_hack,
            phantom: PhantomData,
        }
    }
}

impl<Link: JrLink, Peer: JrPeer, Notif, F, T, ToFut> JrMessageHandler
    for NotificationHandler<Link, Peer, Notif, F, ToFut>
where
    Link: HasPeer<Peer>,
    Notif: JsonRpcNotification,
    F: AsyncFnMut(Notif, JrConnectionCx<Link>) -> Result<T, crate::Error> + Send,
    T: crate::IntoHandled<(Notif, JrConnectionCx<Link>)>,
    ToFut: Fn(&mut F, Notif, JrConnectionCx<Link>) -> crate::BoxFuture<'_, Result<T, crate::Error>>
        + Send
        + Sync,
{
    type Link = Link;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        std::any::type_name::<Notif>()
    }

    async fn handle_message(
        &mut self,
        message_cx: MessageCx,
        connection_cx: JrConnectionCx<Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        handle_incoming_message::<Link, Peer>(
            Peer::default(),
            message_cx,
            connection_cx,
            async |message_cx, connection_cx| {
                match message_cx {
                    MessageCx::Notification(message) => {
                        tracing::debug!(
                            request_type = std::any::type_name::<Notif>(),
                            message = ?message,
                            "NotificationHandler::handle_message"
                        );
                        if !Notif::matches_method(&message.method) {
                            tracing::trace!(
                                "NotificationHandler::handle_notification: method doesn't match"
                            );
                            Ok(Handled::No {
                                message: MessageCx::Notification(message),
                                retry: false,
                            })
                        } else {
                            match Notif::parse_message(&message.method, &message.params) {
                                Ok(notif) => {
                                    tracing::trace!(
                                        ?notif,
                                        "NotificationHandler::handle_notification: parse completed"
                                    );
                                    let result = (self.to_future_hack)(
                                        &mut self.handler,
                                        notif,
                                        connection_cx,
                                    )
                                    .await?;
                                    match result.into_handled() {
                                        Handled::Yes => Ok(Handled::Yes),
                                        Handled::No {
                                            message: (notification, _cx),
                                            retry,
                                        } => {
                                            // Handler returned the notification back, convert to untyped
                                            let untyped = notification.to_untyped_message()?;
                                            Ok(Handled::No {
                                                message: MessageCx::Notification(untyped),
                                                retry,
                                            })
                                        }
                                    }
                                }
                                Err(err) => {
                                    tracing::trace!(
                                        ?err,
                                        "NotificationHandler::handle_notification: parse errored"
                                    );
                                    Err(err)
                                }
                            }
                        }
                    }

                    MessageCx::Request(..) | MessageCx::Response(..) => Ok(Handled::No {
                        message: message_cx,
                        retry: false,
                    }),
                }
            },
        )
        .await
    }
}

/// Handler that handles both requests and notifications of specific types.
pub struct MessageHandler<
    Link: JrLink,
    Peer: JrPeer,
    Req: JsonRpcRequest = UntypedMessage,
    Notif: JsonRpcNotification = UntypedMessage,
    F = (),
    ToFut = (),
> {
    handler: F,
    to_future_hack: ToFut,
    phantom: PhantomData<fn(Link, Peer, Req, Notif)>,
}

impl<Link: JrLink, Peer: JrPeer, Req: JsonRpcRequest, Notif: JsonRpcNotification, F, ToFut>
    MessageHandler<Link, Peer, Req, Notif, F, ToFut>
{
    /// Creates a new message handler
    pub fn new(_peer: Peer, _link: Link, handler: F, to_future_hack: ToFut) -> Self {
        Self {
            handler,
            to_future_hack,
            phantom: PhantomData,
        }
    }
}

impl<Link: JrLink, Peer: JrPeer, Req: JsonRpcRequest, Notif: JsonRpcNotification, F, T, ToFut>
    JrMessageHandler for MessageHandler<Link, Peer, Req, Notif, F, ToFut>
where
    Link: HasPeer<Peer>,
    F: AsyncFnMut(MessageCx<Req, Notif>, JrConnectionCx<Link>) -> Result<T, crate::Error> + Send,
    T: IntoHandled<MessageCx<Req, Notif>>,
    ToFut: Fn(
            &mut F,
            MessageCx<Req, Notif>,
            JrConnectionCx<Link>,
        ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
        + Send
        + Sync,
{
    type Link = Link;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!(
            "({}, {})",
            std::any::type_name::<Req>(),
            std::any::type_name::<Notif>()
        )
    }

    async fn handle_message(
        &mut self,
        message_cx: MessageCx,
        connection_cx: JrConnectionCx<Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        handle_incoming_message::<Link, Peer>(
            Peer::default(),
            message_cx,
            connection_cx,
            async |message_cx, connection_cx| match message_cx
                .into_typed_message_cx::<Req, Notif>()?
            {
                Ok(typed_message_cx) => {
                    let result =
                        (self.to_future_hack)(&mut self.handler, typed_message_cx, connection_cx)
                            .await?;
                    match result.into_handled() {
                        Handled::Yes => Ok(Handled::Yes),
                        Handled::No {
                            message: MessageCx::Request(request, request_cx),
                            retry,
                        } => {
                            let untyped = request.to_untyped_message()?;
                            Ok(Handled::No {
                                message: MessageCx::Request(untyped, request_cx.erase_to_json()),
                                retry,
                            })
                        }
                        Handled::No {
                            message: MessageCx::Notification(notification),
                            retry,
                        } => {
                            let untyped = notification.to_untyped_message()?;
                            Ok(Handled::No {
                                message: MessageCx::Notification(untyped),
                                retry,
                            })
                        }
                        Handled::No {
                            message: MessageCx::Response(result, request_cx),
                            retry,
                        } => {
                            let method = request_cx.method();
                            let untyped_result = match result {
                                Ok(response) => response.into_json(method).map(Ok),
                                Err(err) => Ok(Err(err)),
                            }?;
                            Ok(Handled::No {
                                message: MessageCx::Response(
                                    untyped_result,
                                    request_cx.erase_to_json(),
                                ),
                                retry,
                            })
                        }
                    }
                }

                Err(message_cx) => Ok(Handled::No {
                    message: message_cx,
                    retry: false,
                }),
            },
        )
        .await
    }
}

/// Wraps a handler with an optional name for tracing/debugging.
pub struct NamedHandler<H> {
    name: Option<String>,
    handler: H,
}

impl<H: JrMessageHandler> NamedHandler<H> {
    /// Creates a new named handler
    pub fn new(name: Option<String>, handler: H) -> Self {
        Self { name, handler }
    }
}

impl<H: JrMessageHandler> JrMessageHandler for NamedHandler<H> {
    type Link = H::Link;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!(
            "NamedHandler({:?}, {:?})",
            self.name,
            self.handler.describe_chain()
        )
    }

    async fn handle_message(
        &mut self,
        message: MessageCx,
        connection_cx: JrConnectionCx<H::Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        if let Some(name) = &self.name {
            crate::util::instrumented_with_connection_name(
                name.clone(),
                self.handler.handle_message(message, connection_cx),
            )
            .await
        } else {
            self.handler.handle_message(message, connection_cx).await
        }
    }
}

/// Chains two handlers together, trying the first handler and falling back to the second
pub struct ChainedHandler<H1, H2> {
    handler1: H1,
    handler2: H2,
}

impl<H1, H2> ChainedHandler<H1, H2>
where
    H1: JrMessageHandler,
    H2: JrMessageHandler<Link = H1::Link>,
{
    /// Creates a new chain handler
    pub fn new(handler1: H1, handler2: H2) -> Self {
        Self { handler1, handler2 }
    }
}

impl<H1, H2> JrMessageHandler for ChainedHandler<H1, H2>
where
    H1: JrMessageHandler,
    H2: JrMessageHandler<Link = H1::Link>,
{
    type Link = H1::Link;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        format!(
            "{:?}, {:?}",
            self.handler1.describe_chain(),
            self.handler2.describe_chain()
        )
    }

    async fn handle_message(
        &mut self,
        message: MessageCx,
        connection_cx: JrConnectionCx<H1::Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        match self
            .handler1
            .handle_message(message, connection_cx.clone())
            .await?
        {
            Handled::Yes => Ok(Handled::Yes),
            Handled::No {
                message,
                retry: retry1,
            } => match self.handler2.handle_message(message, connection_cx).await? {
                Handled::Yes => Ok(Handled::Yes),
                Handled::No {
                    message,
                    retry: retry2,
                } => Ok(Handled::No {
                    message,
                    retry: retry1 | retry2,
                }),
            },
        }
    }
}
