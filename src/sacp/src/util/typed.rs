//! Utilities for pattern matching on untyped JSON-RPC messages.
//!
//! When handling [`UntypedMessage`]s, you can use [`TypeRequest`] and [`TypeNotification`]
//! to create a pattern-matching flow that tries to parse messages as specific types,
//! falling back to a default handler if no type matches.

// Types re-exported from crate root
use jsonrpcmsg::Params;

use crate::{
    Handled, JrConnectionCx, JrNotification, JrRequest, JrRequestCx, MessageAndCx, UntypedMessage,
    util::json_cast,
};

/// Helper for pattern-matching on untyped JSON-RPC requests.
///
/// Use this when you receive an [`UntypedMessage`] representing a request and want to
/// try parsing it as different concrete types, handling whichever type matches.
///
/// This is very similar to using [`JrHandlerChain::apply`] except that each match
/// executes immediately, which can help avoid borrow check errors.
///
/// # Example
///
/// ```
/// # use sacp::{UntypedMessage, JrRequestCx};
/// # use sacp::schema::{InitializeRequest, InitializeResponse, PromptRequest, PromptResponse};
/// # use sacp::util::TypeRequest;
/// # async fn example(message: UntypedMessage, cx: JrRequestCx<serde_json::Value>) -> Result<(), sacp::Error> {
/// TypeRequest::new(message, cx)
///     .handle_if(|req: InitializeRequest, cx: JrRequestCx<InitializeResponse>| async move {
///         // Handle initialization
///         let response: InitializeResponse = todo!();
///         cx.respond(response)
///     })
///     .await
///     .handle_if(|req: PromptRequest, cx: JrRequestCx<PromptResponse>| async move {
///         // Handle prompts
///         let response: PromptResponse = todo!();
///         cx.respond(response)
///     })
///     .await
///     .otherwise(|untyped: UntypedMessage, cx: JrRequestCx<serde_json::Value>| async move {
///         // Fallback for unrecognized requests
///         cx.respond_with_error(sacp::util::internal_error("unknown method"))
///     })
///     .await
/// # }
/// ```
///
/// Each `handle_if` tries to parse the message as the specified type. If parsing succeeds,
/// that handler runs and subsequent handlers are skipped. If parsing fails for all types,
/// the `otherwise` handler receives the original untyped message.
#[must_use]
pub struct MatchMessage {
    state: Result<Handled<MessageAndCx>, crate::Error>,
}

impl MatchMessage {
    /// Create a new pattern matcher for the given untyped request message.
    pub fn new(message: MessageAndCx) -> Self {
        Self {
            state: Ok(Handled::No(message)),
        }
    }

    /// Try to handle the message as a request of type `R`.
    ///
    /// If the message can be parsed as `R`, the handler `op` is called with the parsed
    /// request and a typed request context. If parsing fails or the message was already
    /// handled by a previous `handle_if`, this call has no effect.
    ///
    /// Returns `self` to allow chaining multiple `handle_if` calls.
    pub async fn if_request<R: JrRequest>(
        mut self,
        op: impl AsyncFnOnce(R, JrRequestCx<R::Response>) -> Result<(), crate::Error>,
    ) -> Self {
        if let Ok(Handled::No(MessageAndCx::Request(untyped_request, untyped_request_cx))) =
            self.state
        {
            match R::parse_request(untyped_request.method(), untyped_request.params()) {
                Some(Ok(typed_request)) => match op(typed_request, untyped_request_cx.cast()).await
                {
                    Ok(()) => self.state = Ok(Handled::Yes),
                    Err(err) => self.state = Err(err),
                },
                Some(Err(err)) => self.state = Err(err),
                None => {
                    self.state = Ok(Handled::No(MessageAndCx::Request(
                        untyped_request,
                        untyped_request_cx,
                    )));
                }
            }
        }
        self
    }

    /// Try to handle the message as a notification of type `R`.
    ///
    /// If the message can be parsed as `R`, the handler `op` is called with the parsed
    /// request and notification context. If parsing fails or the message was already
    /// handled by a previous `handle_if`, this call has no effect.
    ///
    /// Returns `self` to allow chaining multiple `handle_if` calls.
    pub async fn if_notification<N: JrNotification>(
        mut self,
        op: impl AsyncFnOnce(N, JrConnectionCx) -> Result<(), crate::Error>,
    ) -> Self {
        if let Ok(Handled::No(MessageAndCx::Notification(untyped_notification, notification_cx))) =
            self.state
        {
            match N::parse_notification(
                untyped_notification.method(),
                untyped_notification.params(),
            ) {
                Some(Ok(typed_notification)) => match op(typed_notification, notification_cx).await
                {
                    Ok(()) => self.state = Ok(Handled::Yes),
                    Err(err) => self.state = Err(err),
                },
                Some(Err(err)) => self.state = Err(err),
                None => {
                    self.state = Ok(Handled::No(MessageAndCx::Notification(
                        untyped_notification,
                        notification_cx,
                    )));
                }
            }
        }
        self
    }

    /// Handle messages that didn't match any previous `handle_if` call.
    ///
    /// This is the fallback handler that receives the original untyped message if none
    /// of the typed handlers matched. You must call this method to complete the pattern
    /// matching chain and get the final result.
    pub async fn otherwise(
        self,
        op: impl AsyncFnOnce(MessageAndCx) -> Result<(), crate::Error>,
    ) -> Result<(), crate::Error> {
        match self.state {
            Ok(Handled::Yes) => Ok(()),
            Ok(Handled::No(message)) => op(message).await,
            Err(err) => Err(err),
        }
    }
}

/// Builder for pattern-matching on untyped JSON-RPC notifications.
///
/// Similar to [`TypeRequest`] but for notifications (fire-and-forget messages with no response).
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
/// # use sacp::util::TypeNotification;
/// # async fn example(message: UntypedMessage, cx: &JrConnectionCx) -> Result<(), sacp::Error> {
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
pub struct TypeNotification {
    cx: JrConnectionCx,
    state: Option<TypeNotificationState>,
}

enum TypeNotificationState {
    Unhandled(String, Option<Params>),
    Handled(Result<(), crate::Error>),
}

impl TypeNotification {
    /// Create a new pattern matcher for the given untyped notification message.
    pub fn new(request: UntypedMessage, cx: &JrConnectionCx) -> Self {
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
                match N::parse_notification(&method, &params) {
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
