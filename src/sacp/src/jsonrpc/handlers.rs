use crate::jsonrpc::dynamic_handler::DynamicHandler;
use crate::jsonrpc::{Handled, IntoHandled, JrMessageHandler};
use crate::role::{HasEndpoint, JrEndpoint, JrRole};
use crate::{JrConnectionCx, JrNotification, JrRequest, MessageCx, UntypedMessage};
// Types re-exported from crate root
use super::JrRequestCx;
use std::marker::PhantomData;
use std::ops::AsyncFnMut;

/// Handler that iterates through a vector of dynamic handlers.
pub struct VecHandler<'scope, Role: JrRole> {
    handlers: Vec<Box<dyn DynamicHandler<Role> + 'scope>>,
}

impl<'scope, Role: JrRole> VecHandler<'scope, Role> {
    /// Creates a new VecHandler with the given handlers.
    pub fn new(handlers: Vec<Box<dyn DynamicHandler<Role> + 'scope>>) -> Self {
        Self { handlers }
    }
}

impl<'scope, Role: JrRole> JrMessageHandler for VecHandler<'scope, Role> {
    type Role = Role;

    async fn handle_message(
        &mut self,
        mut message: MessageCx,
        cx: JrConnectionCx<Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        let mut retry_any = false;
        for handler in &mut self.handlers {
            match handler.dyn_handle_message(message, cx.clone()).await? {
                Handled::Yes => return Ok(Handled::Yes),
                Handled::No { message: m, retry } => {
                    message = m;
                    retry_any |= retry;
                }
            }
        }
        Ok(Handled::No {
            message,
            retry: retry_any,
        })
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        let descriptions: Vec<String> = self
            .handlers
            .iter()
            .map(|h| h.dyn_describe_chain())
            .collect();
        descriptions
    }
}

/// Null handler that accepts no messages.
pub struct NullHandler<Role: JrRole> {
    role: Role,
}

impl<Role: JrRole> NullHandler<Role> {
    /// Creates a new null handler.
    pub fn new(role: Role) -> Self {
        Self { role }
    }

    /// Returns the role.
    pub fn role(&self) -> Role {
        self.role
    }
}

impl<Role: JrRole> JrMessageHandler for NullHandler<Role> {
    type Role = Role;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "(null)"
    }

    async fn handle_message(
        &mut self,
        message: MessageCx,
        _cx: JrConnectionCx<Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        Ok(Handled::No {
            message,
            retry: false,
        })
    }
}

/// Handler for typed request messages
pub struct RequestHandler<
    Role: JrRole,
    End: JrEndpoint,
    Req: JrRequest = UntypedMessage,
    F = (),
    ToFut = (),
> {
    handler: F,
    to_future_hack: ToFut,
    role: Role,
    phantom: PhantomData<fn(End, Req)>,
}

impl<Role: JrRole, End: JrEndpoint, Req: JrRequest, F, ToFut>
    RequestHandler<Role, End, Req, F, ToFut>
{
    /// Creates a new request handler
    pub fn new(_endpoint: End, role: Role, handler: F, to_future_hack: ToFut) -> Self {
        Self {
            handler,
            to_future_hack,
            role,
            phantom: PhantomData,
        }
    }

    /// Returns the role.
    pub fn role(&self) -> Role {
        self.role
    }
}

impl<Role: JrRole, End: JrEndpoint, Req, F, T, ToFut> JrMessageHandler
    for RequestHandler<Role, End, Req, F, ToFut>
where
    Role: HasEndpoint<End>,
    Req: JrRequest,
    F: AsyncFnMut(Req, JrRequestCx<Req::Response>, JrConnectionCx<Role>) -> Result<T, crate::Error>
        + Send,
    T: crate::IntoHandled<(Req, JrRequestCx<Req::Response>)>,
    ToFut: Fn(
            &mut F,
            Req,
            JrRequestCx<Req::Response>,
            JrConnectionCx<Role>,
        ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
        + Send
        + Sync,
{
    type Role = Role;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        std::any::type_name::<Req>()
    }

    async fn handle_message(
        &mut self,
        message_cx: MessageCx,
        connection_cx: JrConnectionCx<Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        let remote_style = Role::remote_style(End::default());
        remote_style
            .handle_incoming_message(
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
                            match Req::parse_message(&message.method, &message.params) {
                                Some(Ok(req)) => {
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
                                Some(Err(err)) => {
                                    tracing::trace!(
                                        ?err,
                                        "RequestHandler::handle_request: parse errored"
                                    );
                                    Err(err)
                                }
                                None => {
                                    tracing::trace!("RequestHandler::handle_request: parse failed");
                                    Ok(Handled::No {
                                        message: MessageCx::Request(message, request_cx),
                                        retry: false,
                                    })
                                }
                            }
                        }

                        MessageCx::Notification(..) => Ok(Handled::No {
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
    Role: JrRole,
    End: JrEndpoint,
    Notif: JrNotification = UntypedMessage,
    F = (),
    ToFut = (),
> {
    handler: F,
    to_future_hack: ToFut,
    role: Role,
    phantom: PhantomData<fn(End, Notif)>,
}

impl<Role: JrRole, End: JrEndpoint, Notif: JrNotification, F, ToFut>
    NotificationHandler<Role, End, Notif, F, ToFut>
{
    /// Creates a new notification handler
    pub fn new(_endpoint: End, role: Role, handler: F, to_future_hack: ToFut) -> Self {
        Self {
            handler,
            to_future_hack,
            role,
            phantom: PhantomData,
        }
    }

    /// Returns the role.
    pub fn role(&self) -> Role {
        self.role
    }
}

impl<Role: JrRole, End: JrEndpoint, Notif, F, T, ToFut> JrMessageHandler
    for NotificationHandler<Role, End, Notif, F, ToFut>
where
    Role: HasEndpoint<End>,
    Notif: JrNotification,
    F: AsyncFnMut(Notif, JrConnectionCx<Role>) -> Result<T, crate::Error> + Send,
    T: crate::IntoHandled<(Notif, JrConnectionCx<Role>)>,
    ToFut: Fn(&mut F, Notif, JrConnectionCx<Role>) -> crate::BoxFuture<'_, Result<T, crate::Error>>
        + Send
        + Sync,
{
    type Role = Role;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        std::any::type_name::<Notif>()
    }

    async fn handle_message(
        &mut self,
        message_cx: MessageCx,
        connection_cx: JrConnectionCx<Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        let remote_style = Role::remote_style(End::default());
        remote_style
            .handle_incoming_message(
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
                            match Notif::parse_message(&message.method, &message.params) {
                                Some(Ok(notif)) => {
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
                                Some(Err(err)) => {
                                    tracing::trace!(
                                        ?err,
                                        "NotificationHandler::handle_notification: parse errored"
                                    );
                                    Err(err)
                                }
                                None => {
                                    tracing::trace!(
                                        "NotificationHandler::handle_notification: parse failed"
                                    );
                                    Ok(Handled::No {
                                        message: MessageCx::Notification(message),
                                        retry: false,
                                    })
                                }
                            }
                        }

                        MessageCx::Request(..) => Ok(Handled::No {
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
    Role: JrRole,
    End: JrEndpoint,
    Req: JrRequest = UntypedMessage,
    Notif: JrNotification = UntypedMessage,
    F = (),
    ToFut = (),
> {
    handler: F,
    to_future_hack: ToFut,
    role: Role,
    phantom: PhantomData<fn(End, Req, Notif)>,
}

impl<Role: JrRole, End: JrEndpoint, Req: JrRequest, Notif: JrNotification, F, T, ToFut>
    MessageHandler<Role, End, Req, Notif, F, ToFut>
where
    F: AsyncFnMut(MessageCx<Req, Notif>, JrConnectionCx<Role>) -> Result<T, crate::Error>,
    T: IntoHandled<MessageCx<Req, Notif>>,
{
    /// Creates a new message handler
    pub fn new(_endpoint: End, role: Role, handler: F, to_future_hack: ToFut) -> Self {
        Self {
            handler,
            to_future_hack,
            role,
            phantom: PhantomData,
        }
    }

    /// Returns the role.
    pub fn role(&self) -> Role {
        self.role
    }
}

impl<Role: JrRole, End: JrEndpoint, Req: JrRequest, Notif: JrNotification, F, T, ToFut>
    JrMessageHandler for MessageHandler<Role, End, Req, Notif, F, ToFut>
where
    Role: HasEndpoint<End>,
    F: AsyncFnMut(MessageCx<Req, Notif>, JrConnectionCx<Role>) -> Result<T, crate::Error> + Send,
    T: IntoHandled<MessageCx<Req, Notif>>,
    ToFut: Fn(
            &mut F,
            MessageCx<Req, Notif>,
            JrConnectionCx<Role>,
        ) -> crate::BoxFuture<'_, Result<T, crate::Error>>
        + Send
        + Sync,
{
    type Role = Role;

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
        connection_cx: JrConnectionCx<Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        let remote_style = Role::remote_style(End::default());
        remote_style
            .handle_incoming_message(
                message_cx,
                connection_cx,
                async |message_cx, connection_cx| match message_cx
                    .into_typed_message_cx::<Req, Notif>()?
                {
                    Ok(typed_message_cx) => {
                        let result = (self.to_future_hack)(
                            &mut self.handler,
                            typed_message_cx,
                            connection_cx,
                        )
                        .await?;
                        match result.into_handled() {
                            Handled::Yes => Ok(Handled::Yes),
                            Handled::No {
                                message: MessageCx::Request(request, request_cx),
                                retry,
                            } => {
                                let untyped = request.to_untyped_message()?;
                                Ok(Handled::No {
                                    message: MessageCx::Request(
                                        untyped,
                                        request_cx.erase_to_json(),
                                    ),
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
    type Role = H::Role;

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
        connection_cx: JrConnectionCx<H::Role>,
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
