// Types re-exported from crate root

mod typed;
pub use typed::{MatchMessage, MatchMessageFrom, TypeNotification};

/// Cast from `N` to `M` by serializing/deserialization to/from JSON.
pub fn json_cast<N, M>(params: N) -> Result<M, crate::Error>
where
    N: serde::Serialize,
    M: serde::de::DeserializeOwned,
{
    let json = serde_json::to_value(params).map_err(|e| {
        crate::Error::parse_error().with_data(serde_json::json!({
            "error": e.to_string(),
            "phase": "serialization"
        }))
    })?;
    let m = serde_json::from_value(json.clone()).map_err(|e| {
        crate::Error::parse_error().with_data(serde_json::json!({
            "error": e.to_string(),
            "json": json,
            "phase": "deserialization"
        }))
    })?;
    Ok(m)
}

/// Creates an internal error with the given message
pub fn internal_error(message: impl ToString) -> crate::Error {
    crate::Error::internal_error().with_data(message.to_string())
}

/// Creates a parse error with the given message
pub fn parse_error(message: impl ToString) -> crate::Error {
    crate::Error::parse_error().with_data(message.to_string())
}

pub(crate) fn instrumented_with_connection_name<F>(
    name: String,
    task: F,
) -> tracing::instrument::Instrumented<F> {
    use tracing::Instrument;

    task.instrument(tracing::info_span!("connection", name = name))
}

pub(crate) async fn instrument_with_connection_name<R>(
    name: Option<String>,
    task: impl Future<Output = R>,
) -> R {
    if let Some(name) = name {
        instrumented_with_connection_name(name.clone(), task).await
    } else {
        task.await
    }
}

/// Convert a `crate::Error` into a `crate::jsonrpcmsg::Error`
pub fn into_jsonrpc_error(err: crate::Error) -> crate::jsonrpcmsg::Error {
    crate::jsonrpcmsg::Error {
        code: err.code,
        message: err.message,
        data: err.data,
    }
}

/// Run two fallible futures concurrently, returning when both complete successfully
/// or when either fails.
pub async fn both<E>(
    a: impl Future<Output = Result<(), E>>,
    b: impl Future<Output = Result<(), E>>,
) -> Result<(), E> {
    let ((), ()) = futures::future::try_join(a, b).await?;
    Ok(())
}

/// Run `background` until `foreground` completes.
///
/// Returns the result of `foreground`. If `background` errors before
/// `foreground` completes, the error is propagated. If `background`
/// completes with `Ok(())`, we continue waiting for `foreground`.
pub async fn run_until<T, E>(
    background: impl Future<Output = Result<(), E>>,
    foreground: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    use futures::future::{Either, select};
    use std::pin::pin;

    match select(pin!(background), pin!(foreground)).await {
        Either::Left((bg_result, fg_future)) => {
            // Background finished first
            bg_result?; // propagate error, or if Ok(()), keep waiting
            fg_future.await
        }
        Either::Right((fg_result, _bg_future)) => {
            // Foreground finished first, drop background
            fg_result
        }
    }
}
