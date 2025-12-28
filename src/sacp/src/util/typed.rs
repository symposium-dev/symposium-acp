//! Utilities for pattern matching on untyped JSON-RPC messages.
//!
//! When handling [`UntypedMessage`]s, you can use [`MatchMessage`] for simple parsing
//! or [`MatchMessageFrom`] when you need peer-aware transforms (e.g., unwrapping
//! proxy envelopes).
//!
//! # When to use which
//!
//! - **[`MatchMessageFrom`]**: Preferred over implementing [`JrMessageHandler`] directly.
//!   Use this in connection handlers when you need to match on message types with
//!   proper peer-aware transforms (e.g., unwrapping `SuccessorMessage` envelopes).
//!
//! - **[`MatchMessage`]**: Use this when you already have an unwrapped message and
//!   just need to parse it, such as inside a [`MatchMessageFrom`] callback or when
//!   processing messages that don't need peer transforms.
//!
//! [`JrMessageHandler`]: crate::JrMessageHandler

// Types re-exported from crate root
use jsonrpcmsg::Params;

use crate::{
    Handled, HasDefaultPeer, JrConnectionCx, JrMessageHandler, JrNotification, JrRequest,
    JrRequestCx, MessageCx, UntypedMessage,
    link::{HasPeer, JrLink},
    peer::JrPeer,
    util::json_cast,
};

/// Role-agnostic helper for pattern-matching on untyped JSON-RPC messages.
///
/// Use this when you already have an unwrapped message and just need to parse it,
/// such as inside a [`MatchMessageFrom`] callback or when processing messages
/// that don't need peer transforms.
///
/// For connection handlers where you need proper peer-aware transforms,
/// use [`MatchMessageFrom`] instead.
///
/// # Example
///
/// ```
/// # use sacp::MessageCx;
/// # use sacp::schema::{InitializeRequest, InitializeResponse};
/// # use sacp::util::MatchMessage;
/// # async fn example(message: MessageCx) -> Result<(), sacp::Error> {
/// MatchMessage::new(message)
///     .if_request(|req: InitializeRequest, request_cx: sacp::JrRequestCx<InitializeResponse>| async move {
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
///     .otherwise(|message| async move {
///         match message {
///             MessageCx::Request(_, request_cx) => {
///                 request_cx.respond_with_error(sacp::util::internal_error("unknown method"))
///             }
///             MessageCx::Notification(_) => Ok(()),
///         }
///     })
///     .await
/// # }
/// ```
#[must_use]
pub struct MatchMessage {
    state: Result<Handled<MessageCx>, crate::Error>,
}

impl MatchMessage {
    /// Create a new pattern matcher for the given message.
    pub fn new(message: MessageCx) -> Self {
        Self {
            state: Ok(Handled::No {
                message,
                retry: false,
            }),
        }
    }

    /// Create a pattern matcher from an existing `Handled` state.
    ///
    /// This is useful when composing with [`MatchMessageFrom`] which applies
    /// peer transforms before delegating to `MatchMessage` for parsing.
    pub fn from_handled(state: Result<Handled<MessageCx>, crate::Error>) -> Self {
        Self { state }
    }

    /// Try to handle the message as a request of type `Req`.
    ///
    /// If the message can be parsed as `Req`, the handler `op` is called with the parsed
    /// request and a typed request context. If parsing fails or the message was already
    /// handled by a previous call, this has no effect.
    pub async fn if_request<Req: JrRequest, H>(
        mut self,
        op: impl AsyncFnOnce(Req, JrRequestCx<Req::Response>) -> Result<H, crate::Error>,
    ) -> Self
    where
        H: crate::IntoHandled<(Req, JrRequestCx<Req::Response>)>,
    {
        if let Ok(Handled::No {
            message: message_cx,
            retry,
        }) = self.state
        {
            self.state = match message_cx {
                MessageCx::Request(untyped_request, untyped_request_cx) => {
                    match Req::parse_message(untyped_request.method(), untyped_request.params()) {
                        Some(Ok(typed_request)) => {
                            let typed_request_cx = untyped_request_cx.cast();
                            match op(typed_request, typed_request_cx).await {
                                Ok(result) => match result.into_handled() {
                                    Handled::Yes => Ok(Handled::Yes),
                                    Handled::No {
                                        message: (request, request_cx),
                                        retry: request_retry,
                                    } => match request.to_untyped_message() {
                                        Ok(untyped) => Ok(Handled::No {
                                            message: MessageCx::Request(
                                                untyped,
                                                request_cx.erase_to_json(),
                                            ),
                                            retry: retry | request_retry,
                                        }),
                                        Err(err) => Err(err),
                                    },
                                },
                                Err(err) => Err(err),
                            }
                        }
                        Some(Err(err)) => Err(err),
                        None => Ok(Handled::No {
                            message: MessageCx::Request(untyped_request, untyped_request_cx),
                            retry,
                        }),
                    }
                }
                MessageCx::Notification(_) => Ok(Handled::No {
                    message: message_cx,
                    retry,
                }),
            };
        }
        self
    }

    /// Try to handle the message as a notification of type `N`.
    ///
    /// If the message can be parsed as `N`, the handler `op` is called with the parsed
    /// notification. If parsing fails or the message was already handled, this has no effect.
    pub async fn if_notification<N: JrNotification, H>(
        mut self,
        op: impl AsyncFnOnce(N) -> Result<H, crate::Error>,
    ) -> Self
    where
        H: crate::IntoHandled<N>,
    {
        if let Ok(Handled::No {
            message: message_cx,
            retry,
        }) = self.state
        {
            self.state = match message_cx {
                MessageCx::Notification(untyped_notification) => {
                    match N::parse_message(
                        untyped_notification.method(),
                        untyped_notification.params(),
                    ) {
                        Some(Ok(typed_notification)) => match op(typed_notification).await {
                            Ok(result) => match result.into_handled() {
                                Handled::Yes => Ok(Handled::Yes),
                                Handled::No {
                                    message: notification,
                                    retry: notification_retry,
                                } => match notification.to_untyped_message() {
                                    Ok(untyped) => Ok(Handled::No {
                                        message: MessageCx::Notification(untyped),
                                        retry: retry | notification_retry,
                                    }),
                                    Err(err) => Err(err),
                                },
                            },
                            Err(err) => Err(err),
                        },
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
            };
        }
        self
    }

    /// Try to handle the message as a typed `MessageCx<R, N>`.
    ///
    /// This attempts to parse the message as either request type `R` or notification type `N`,
    /// providing a typed `MessageCx` to the handler if successful.
    pub async fn if_message<R: JrRequest, N: JrNotification, H>(
        mut self,
        op: impl AsyncFnOnce(MessageCx<R, N>) -> Result<H, crate::Error>,
    ) -> Self
    where
        H: crate::IntoHandled<MessageCx<R, N>>,
    {
        if let Ok(Handled::No {
            message: message_cx,
            retry,
        }) = self.state
        {
            self.state = match message_cx.into_typed_message_cx::<R, N>() {
                Ok(Ok(typed_message_cx)) => match op(typed_message_cx).await {
                    Ok(result) => match result.into_handled() {
                        Handled::Yes => Ok(Handled::Yes),
                        Handled::No {
                            message: typed_message_cx,
                            retry: message_retry,
                        } => match typed_message_cx.into_untyped_message_cx() {
                            Ok(untyped) => Ok(Handled::No {
                                message: untyped,
                                retry: retry | message_retry,
                            }),
                            Err(err) => Err(err),
                        },
                    },
                    Err(err) => Err(err),
                },
                Ok(Err(message_cx)) => Ok(Handled::No {
                    message: message_cx,
                    retry,
                }),
                Err(err) => Err(err),
            };
        }
        self
    }

    /// Complete matching, returning `Handled::No` if no match was found.
    pub fn done(self) -> Result<Handled<MessageCx>, crate::Error> {
        self.state
    }

    /// Handle messages that didn't match any previous handler.
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

    /// Handle messages that didn't match any previous handler.
    pub fn otherwise_ignore(self) -> Result<(), crate::Error> {
        match self.state {
            Ok(_) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

/// Role-aware helper for pattern-matching on untyped JSON-RPC requests.
///
/// **Prefer this over implementing [`JrMessageHandler`] directly.** This provides
/// a more ergonomic API for matching on message types in connection handlers.
///
/// Use this when you need peer-aware transforms (e.g., unwrapping proxy envelopes)
/// before parsing messages. For simple parsing without peer awareness (e.g., inside
/// a callback), use [`MatchMessage`] instead.
///
/// This wraps [`MatchMessage`] and applies peer-specific message transformations
/// via `remote_style().handle_incoming_message()` before delegating to `MatchMessage`
/// for the actual parsing.
///
/// [`JrMessageHandler`]: crate::JrMessageHandler
///
/// # Example
///
/// ```
/// # use sacp::MessageCx;
/// # use sacp::schema::{InitializeRequest, InitializeResponse, PromptRequest, PromptResponse};
/// # use sacp::util::MatchMessageFrom;
/// # async fn example(message: MessageCx, cx: &sacp::JrConnectionCx<sacp::AgentToClient>) -> Result<(), sacp::Error> {
/// MatchMessageFrom::new(message, cx)
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
#[must_use]
pub struct MatchMessageFrom<Link: JrLink> {
    state: Result<Handled<MessageCx>, crate::Error>,
    cx: JrConnectionCx<Link>,
}

impl<Link: JrLink> MatchMessageFrom<Link> {
    /// Create a new pattern matcher for the given untyped request message.
    pub fn new(message: MessageCx, cx: &JrConnectionCx<Link>) -> Self {
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
        Link: HasDefaultPeer,
        H: crate::IntoHandled<(Req, JrRequestCx<Req::Response>)>,
    {
        self.if_request_from(<Link::DefaultPeer>::default(), op)
            .await
    }

    /// Try to handle the message as a request of type `Req` from a specific peer.
    ///
    /// This is similar to [`if_request`](Self::if_request), but first applies peer-specific
    /// message transformation (e.g., unwrapping `SuccessorMessage` envelopes when receiving
    /// from an agent via a proxy).
    ///
    /// # Parameters
    ///
    /// * `peer` - The peer the message is expected to come from
    /// * `op` - The handler to call if the message matches
    pub async fn if_request_from<Peer: JrPeer, Req: JrRequest, H>(
        mut self,
        peer: Peer,
        op: impl AsyncFnOnce(Req, JrRequestCx<Req::Response>) -> Result<H, crate::Error>,
    ) -> Self
    where
        Link: HasPeer<Peer>,
        H: crate::IntoHandled<(Req, JrRequestCx<Req::Response>)>,
    {
        if let Ok(Handled::No { message, retry: _ }) = self.state {
            let remote_style = Link::remote_style(peer);
            self.state = remote_style
                .handle_incoming_message(
                    message,
                    self.cx.clone(),
                    async |message_cx, _connection_cx| {
                        // Delegate to MatchMessage for parsing
                        MatchMessage::new(message_cx).if_request(op).await.done()
                    },
                )
                .await;
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
        Link: HasDefaultPeer,
        H: crate::IntoHandled<N>,
    {
        self.if_notification_from(<Link as HasDefaultPeer>::DefaultPeer::default(), op)
            .await
    }

    /// Try to handle the message as a notification of type `N` from a specific peer.
    ///
    /// This is similar to [`if_notification`](Self::if_notification), but first applies peer-specific
    /// message transformation (e.g., unwrapping `SuccessorMessage` envelopes when receiving
    /// from an agent via a proxy).
    ///
    /// # Parameters
    ///
    /// * `peer` - The peer the message is expected to come from
    /// * `op` - The handler to call if the message matches
    pub async fn if_notification_from<Peer: JrPeer, N: JrNotification, H>(
        mut self,
        peer: Peer,
        op: impl AsyncFnOnce(N) -> Result<H, crate::Error>,
    ) -> Self
    where
        Link: HasPeer<Peer>,
        H: crate::IntoHandled<N>,
    {
        if let Ok(Handled::No { message, retry: _ }) = self.state {
            let remote_style = Link::remote_style(peer);
            self.state = remote_style
                .handle_incoming_message(
                    message,
                    self.cx.clone(),
                    async |message_cx, _connection_cx| {
                        // Delegate to MatchMessage for parsing
                        MatchMessage::new(message_cx)
                            .if_notification(op)
                            .await
                            .done()
                    },
                )
                .await;
        }
        self
    }

    /// Try to handle the message as a typed `MessageCx<R, N>` from a specific peer.
    ///
    /// This is similar to [`MatchMessage::if_message`], but first applies peer-specific
    /// message transformation (e.g., unwrapping `SuccessorMessage` envelopes).
    ///
    /// # Parameters
    ///
    /// * `peer` - The peer the message is expected to come from
    /// * `op` - The handler to call if the message matches
    pub async fn if_message_from<Peer: JrPeer, R: JrRequest, N: JrNotification, H>(
        mut self,
        peer: Peer,
        op: impl AsyncFnOnce(MessageCx<R, N>) -> Result<H, crate::Error>,
    ) -> Self
    where
        Link: HasPeer<Peer>,
        H: crate::IntoHandled<MessageCx<R, N>>,
    {
        if let Ok(Handled::No { message, retry: _ }) = self.state {
            let remote_style = Link::remote_style(peer);
            self.state = remote_style
                .handle_incoming_message(
                    message,
                    self.cx.clone(),
                    async |message_cx, _connection_cx| {
                        // Delegate to MatchMessage for parsing
                        MatchMessage::new(message_cx).if_message(op).await.done()
                    },
                )
                .await;
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

    /// Handle messages that didn't match any previous `handle_if` call.
    ///
    /// This is the fallback handler that receives the original untyped message if none
    /// of the typed handlers matched. You must call this method to complete the pattern
    /// matching chain and get the final result.
    pub async fn otherwise_delegate(
        self,
        mut handler: impl JrMessageHandler<Link = Link>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        match self.state? {
            Handled::Yes => Ok(Handled::Yes),
            Handled::No {
                message,
                retry: outer_retry,
            } => match handler.handle_message(message, self.cx).await? {
                Handled::Yes => Ok(Handled::Yes),
                Handled::No {
                    message,
                    retry: inner_retry,
                } => Ok(Handled::No {
                    message,
                    retry: inner_retry | outer_retry,
                }),
            },
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
pub struct TypeNotification<Link: JrLink> {
    cx: JrConnectionCx<Link>,
    state: Option<TypeNotificationState>,
}

enum TypeNotificationState {
    Unhandled(String, Option<Params>),
    Handled(Result<(), crate::Error>),
}

impl<Link: JrLink> TypeNotification<Link> {
    /// Create a new pattern matcher for the given untyped notification message.
    pub fn new(request: UntypedMessage, cx: &JrConnectionCx<Link>) -> Self {
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
