//! Utilities for pattern matching on untyped JSON-RPC messages.
//!
//! When handling [`UntypedMessage`]s, you can use [`MatchMessage`]
//! to create a pattern-matching flow that tries to parse messages as specific types,
//! falling back to a default handler if no type matches.

// Types re-exported from crate root
use jsonrpcmsg::Params;

use crate::{
    Handled, HasDefaultEndpoint, JrConnectionCx, JrNotification, JrRequest, JrRequestCx, MessageCx,
    UntypedMessage,
    role::{HasEndpoint, JrEndpoint, JrRole},
    util::json_cast,
};

/// Helper for pattern-matching on untyped JSON-RPC requests.
///
/// Use this when you receive an [`UntypedMessage`] representing a request and want to
/// try parsing it as different concrete types, handling whichever type matches.
///
/// This is very similar to using [`JrConnectionBuilder::apply`](`crate::JrConnectionBuilder::apply`) except that each match
/// executes immediately, which can help avoid borrow check errors.
///
/// # Example
///
/// ```
/// # use sacp::MessageCx;
/// # use sacp::schema::{InitializeRequest, InitializeResponse, PromptRequest, PromptResponse};
/// # use sacp::util::MatchMessage;
/// # async fn example(message: MessageCx, cx: &sacp::JrConnectionCx<sacp::AgentToClient>) -> Result<(), sacp::Error> {
/// MatchMessage::new(message, cx)
///     .if_request(|req: InitializeRequest, request_cx: sacp::JrRequestCx<InitializeResponse>| async move {
///         // Handle initialization
///         let response = InitializeResponse {
///             protocol_version: req.protocol_version,
///             agent_capabilities: Default::default(),
///             auth_methods: vec![],
///             meta: None,
///             agent_info: None,
///         };
///         request_cx.respond(response)
///     })
///     .await
///     .if_request(|req: PromptRequest, request_cx: sacp::JrRequestCx<PromptResponse>| async move {
///         // Handle prompts
///         let response = PromptResponse {
///             stop_reason: sacp::schema::StopReason::EndTurn,
///             meta: None,
///         };
///         request_cx.respond(response)
///     })
///     .await
///     .otherwise(|message| async move {
///         // Fallback for unrecognized messages
///         match message {
///             MessageCx::Request(_, request_cx) => request_cx.respond_with_error(sacp::util::internal_error("unknown method")),
///             MessageCx::Notification(_) => Ok(()),
///         }
///     })
///     .await
/// # }
/// ```
///
/// Each `handle_if` tries to parse the message as the specified type. If parsing succeeds,
/// that handler runs and subsequent handlers are skipped. If parsing fails for all types,
/// the `otherwise` handler receives the original untyped message.
#[must_use]
pub struct MatchMessage<Role: JrRole> {
    state: Result<Handled<MessageCx>, crate::Error>,
    cx: JrConnectionCx<Role>,
}

impl<Role: JrRole> MatchMessage<Role> {
    /// Create a new pattern matcher for the given untyped request message.
    pub fn new(message: MessageCx, cx: &JrConnectionCx<Role>) -> Self {
        Self {
            state: Ok(Handled::No {
                message,
                retry: false,
            }),
            cx: cx.clone(),
        }
    }

    /// Try to handle the message as a request of type `Req`.
    ///
    /// If the message can be parsed as `Req`, the handler `op` is called with the parsed
    /// request and a typed request context. If parsing fails or the message was already
    /// handled by a previous `handle_if`, this call has no effect.
    ///
    /// The handler can return either `()` (which becomes `Handled::Yes`) or an explicit
    /// `Handled` value to control whether the message should be passed to the next handler.
    ///
    /// Returns `self` to allow chaining multiple `handle_if` calls.
    pub async fn if_request<Req: JrRequest, H>(
        self,
        op: impl AsyncFnOnce(Req, JrRequestCx<Req::Response>) -> Result<H, crate::Error>,
    ) -> Self
    where
        Role: HasDefaultEndpoint,
        Role: HasEndpoint<<Role as JrRole>::HandlerEndpoint>,
        H: crate::IntoHandled<(Req, JrRequestCx<Req::Response>)>,
    {
        self.if_request_from(<Role::HandlerEndpoint>::default(), op)
            .await
    }

    /// Try to handle the message as a request of type `Req` from a specific endpoint.
    ///
    /// This is similar to [`if_request`](Self::if_request), but first applies endpoint-specific
    /// message transformation (e.g., unwrapping `SuccessorMessage` envelopes when receiving
    /// from an agent via a proxy).
    ///
    /// # Parameters
    ///
    /// * `endpoint` - The endpoint the message is expected to come from
    /// * `connection_cx` - The connection context for the role
    /// * `op` - The handler to call if the message matches
    pub async fn if_request_from<End: JrEndpoint, Req: JrRequest, H>(
        mut self,
        endpoint: End,
        op: impl AsyncFnOnce(Req, JrRequestCx<Req::Response>) -> Result<H, crate::Error>,
    ) -> Self
    where
        Role: HasEndpoint<End>,
        H: crate::IntoHandled<(Req, JrRequestCx<Req::Response>)>,
    {
        if let Ok(Handled::No {
            message: message_cx,
            retry,
        }) = self.state
        {
            let remote_style = Role::remote_style(endpoint);
            match remote_style
                .handle_incoming_message(
                    message_cx,
                    self.cx.clone(),
                    async |message_cx, _connection_cx| match message_cx {
                        MessageCx::Request(untyped_request, untyped_request_cx) => {
                            match Req::parse_message(
                                untyped_request.method(),
                                untyped_request.params(),
                            ) {
                                Some(Ok(typed_request)) => {
                                    let typed_request_cx = untyped_request_cx.cast();
                                    match op(typed_request, typed_request_cx).await {
                                        Ok(result) => match result.into_handled() {
                                            Handled::Yes => Ok(Handled::Yes),
                                            Handled::No {
                                                message: (request, request_cx),
                                                retry: request_retry,
                                            } => {
                                                let untyped = request.to_untyped_message()?;
                                                Ok(Handled::No {
                                                    message: MessageCx::Request(
                                                        untyped,
                                                        request_cx.erase_to_json(),
                                                    ),
                                                    retry: retry | request_retry,
                                                })
                                            }
                                        },
                                        Err(err) => Err(err),
                                    }
                                }
                                Some(Err(err)) => Err(err),
                                None => Ok(Handled::No {
                                    message: MessageCx::Request(
                                        untyped_request,
                                        untyped_request_cx,
                                    ),
                                    retry,
                                }),
                            }
                        }
                        MessageCx::Notification(_) => Ok(Handled::No {
                            message: message_cx,
                            retry,
                        }),
                    },
                )
                .await
            {
                Ok(handled) => self.state = Ok(handled),
                Err(err) => self.state = Err(err),
            }
        }
        self
    }

    /// Try to handle the message as a notification of type `N`.
    ///
    /// If the message can be parsed as `N`, the handler `op` is called with the parsed
    /// notification and connection context. If parsing fails or the message was already
    /// handled by a previous `handle_if`, this call has no effect.
    ///
    /// The handler can return either `()` (which becomes `Handled::Yes`) or an explicit
    /// `Handled` value to control whether the message should be passed to the next handler.
    ///
    /// Returns `self` to allow chaining multiple `handle_if` calls.
    pub async fn if_notification<N: JrNotification, H>(
        self,
        op: impl AsyncFnOnce(N) -> Result<H, crate::Error>,
    ) -> Self
    where
        Role: HasDefaultEndpoint,
        Role: HasEndpoint<<Role as JrRole>::HandlerEndpoint>,
        H: crate::IntoHandled<N>,
    {
        self.if_notification_from(<Role as JrRole>::HandlerEndpoint::default(), op)
            .await
    }

    /// Try to handle the message as a notification of type `N` from a specific endpoint.
    ///
    /// This is similar to [`if_notification`](Self::if_notification), but first applies endpoint-specific
    /// message transformation (e.g., unwrapping `SuccessorMessage` envelopes when receiving
    /// from an agent via a proxy).
    ///
    /// # Parameters
    ///
    /// * `endpoint` - The endpoint the message is expected to come from
    /// * `connection_cx` - The connection context for the role
    /// * `op` - The handler to call if the message matches
    pub async fn if_notification_from<End: JrEndpoint, N: JrNotification, H>(
        mut self,
        endpoint: End,
        op: impl AsyncFnOnce(N) -> Result<H, crate::Error>,
    ) -> Self
    where
        Role: HasEndpoint<End>,
        H: crate::IntoHandled<N>,
    {
        if let Ok(Handled::No {
            message: message_cx,
            retry,
        }) = self.state
        {
            let remote_style = Role::remote_style(endpoint);
            match remote_style
                .handle_incoming_message(
                    message_cx,
                    self.cx.clone(),
                    async |message_cx, _connection_cx| match message_cx {
                        MessageCx::Notification(untyped_notification) => {
                            match N::parse_message(
                                untyped_notification.method(),
                                untyped_notification.params(),
                            ) {
                                Some(Ok(typed_notification)) => {
                                    match op(typed_notification).await {
                                        Ok(result) => match result.into_handled() {
                                            Handled::Yes => Ok(Handled::Yes),
                                            Handled::No {
                                                message: notification,
                                                retry: notification_retry,
                                            } => {
                                                let untyped = notification.to_untyped_message()?;
                                                Ok(Handled::No {
                                                    message: MessageCx::Notification(untyped),
                                                    retry: retry | notification_retry,
                                                })
                                            }
                                        },
                                        Err(err) => Err(err),
                                    }
                                }
                                Some(Err(err)) => Err(err),
                                None => Ok(Handled::No {
                                    message: MessageCx::Notification(untyped_notification),
                                    retry,
                                }),
                            }
                        }
                        MessageCx::Request(_, _) => Ok(Handled::No {
                            message: message_cx,
                            retry,
                        }),
                    },
                )
                .await
            {
                Ok(handled) => self.state = Ok(handled),
                Err(err) => self.state = Err(err),
            }
        }
        self
    }

    /// Complete matching, returning `Handled::No` if no match was found.
    pub async fn if_message_from<End: JrEndpoint, R: JrRequest, N: JrNotification, H>(
        mut self,
        endpoint: End,
        op: impl AsyncFnOnce(MessageCx<R, N>) -> Result<H, crate::Error>,
    ) -> Self
    where
        Role: HasEndpoint<End>,
        H: crate::IntoHandled<MessageCx<R, N>>,
    {
        if let Ok(Handled::No {
            message: message_cx,
            retry,
        }) = self.state
        {
            let remote_style = Role::remote_style(endpoint);
            match remote_style
                .handle_incoming_message(
                    message_cx,
                    self.cx.clone(),
                    async |message_cx, _connection_cx| match message_cx
                        .into_typed_message_cx::<R, N>()?
                    {
                        Ok(typed_message_cx) => match op(typed_message_cx).await {
                            Ok(result) => match result.into_handled() {
                                Handled::Yes => Ok(Handled::Yes),
                                Handled::No {
                                    message: typed_message_cx,
                                    retry: message_retry,
                                } => Ok(Handled::No {
                                    message: typed_message_cx.into_untyped_message_cx()?,
                                    retry: retry | message_retry,
                                }),
                            },
                            Err(err) => Err(err),
                        },

                        Err(message_cx) => Ok(Handled::No {
                            message: message_cx,
                            retry,
                        }),
                    },
                )
                .await
            {
                Ok(handled) => self.state = Ok(handled),
                Err(err) => self.state = Err(err),
            }
        }
        self
    }

    /// Complete matching, returning `Handled::No` if no match was found.
    pub fn done(self) -> Result<Handled<MessageCx>, crate::Error> {
        match self.state {
            Ok(Handled::Yes) => Ok(Handled::Yes),
            Ok(Handled::No { message, retry }) => Ok(Handled::No { message, retry }),
            Err(err) => Err(err),
        }
    }

    /// Handle messages that didn't match any previous `handle_if` call.
    ///
    /// This is the fallback handler that receives the original untyped message if none
    /// of the typed handlers matched. You must call this method to complete the pattern
    /// matching chain and get the final result.
    pub async fn otherwise(
        self,
        op: impl AsyncFnOnce(MessageCx) -> Result<(), crate::Error>,
    ) -> Result<(), crate::Error> {
        match self.state {
            Ok(Handled::Yes) => Ok(()),
            Ok(Handled::No { message, retry: _ }) => op(message).await,
            Err(err) => Err(err),
        }
    }
}

/// Builder for pattern-matching on untyped JSON-RPC notifications.
///
/// Similar to [`MatchMessage`] but specifically for notifications (fire-and-forget messages with no response).
///
/// # Pattern
///
/// The typical pattern is:
/// 1. Create a `TypeNotification` from an untyped message
/// 2. Chain `.handle_if()` calls for each type you want to try
/// 3. End with `.otherwise()` for messages that don't match any type
///
/// # Example
///
/// ```
/// # use sacp::{UntypedMessage, JrConnectionCx};
/// # use sacp::schema::SessionNotification;
/// # use sacp::ClientToAgent;
/// # use sacp::util::TypeNotification;
/// # async fn example(message: UntypedMessage, cx: &JrConnectionCx<ClientToAgent>) -> Result<(), sacp::Error> {
/// TypeNotification::new(message, cx)
///     .handle_if(|notif: SessionNotification| async move {
///         // Handle session notifications
///         println!("Session update: {:?}", notif);
///         Ok(())
///     })
///     .await
///     .otherwise(|untyped: UntypedMessage| async move {
///         // Fallback for unrecognized notifications
///         println!("Unknown notification: {}", untyped.method);
///         Ok(())
///     })
///     .await
/// # }
/// ```
///
/// Since notifications don't expect responses, handlers only receive the parsed
/// notification (not a request context).
#[must_use]
pub struct TypeNotification<Role: JrRole> {
    cx: JrConnectionCx<Role>,
    state: Option<TypeNotificationState>,
}

enum TypeNotificationState {
    Unhandled(String, Option<Params>),
    Handled(Result<(), crate::Error>),
}

impl<Role: JrRole> TypeNotification<Role> {
    /// Create a new pattern matcher for the given untyped notification message.
    pub fn new(request: UntypedMessage, cx: &JrConnectionCx<Role>) -> Self {
        let UntypedMessage { method, params } = request;
        let params: Option<Params> = json_cast(params).expect("valid params");
        Self {
            cx: cx.clone(),
            state: Some(TypeNotificationState::Unhandled(method, params)),
        }
    }

    /// Try to handle the message as type `N`.
    ///
    /// If the message can be parsed as `N`, the handler `op` is called with the parsed
    /// notification. If parsing fails or the message was already handled by a previous
    /// `handle_if`, this call has no effect.
    ///
    /// Returns `self` to allow chaining multiple `handle_if` calls.
    pub async fn handle_if<N: JrNotification>(
        mut self,
        op: impl AsyncFnOnce(N) -> Result<(), crate::Error>,
    ) -> Self {
        self.state = Some(match self.state.take().expect("valid state") {
            TypeNotificationState::Unhandled(method, params) => {
                match N::parse_message(&method, &params) {
                    Some(Ok(request)) => TypeNotificationState::Handled(op(request).await),

                    Some(Err(err)) => {
                        TypeNotificationState::Handled(self.cx.send_error_notification(err))
                    }

                    None => TypeNotificationState::Unhandled(method, params),
                }
            }

            TypeNotificationState::Handled(err) => TypeNotificationState::Handled(err),
        });
        self
    }

    /// Handle messages that didn't match any previous `handle_if` call.
    ///
    /// This is the fallback handler that receives the original untyped message if none
    /// of the typed handlers matched. You must call this method to complete the pattern
    /// matching chain and get the final result.
    pub async fn otherwise(
        mut self,
        op: impl AsyncFnOnce(UntypedMessage) -> Result<(), crate::Error>,
    ) -> Result<(), crate::Error> {
        match self.state.take().expect("valid state") {
            TypeNotificationState::Unhandled(method, params) => {
                match UntypedMessage::new(&method, params) {
                    Ok(m) => op(m).await,
                    Err(err) => self.cx.send_error_notification(err),
                }
            }
            TypeNotificationState::Handled(r) => r,
        }
    }
}
