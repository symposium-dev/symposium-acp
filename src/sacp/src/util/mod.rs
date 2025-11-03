// Types re-exported from crate root

mod typed;
pub use typed::{TypeNotification, TypeRequest};

/// Cast from `N` to `M` by serializing/deserialization to/from JSON.
pub fn json_cast<N, M>(params: N) -> Result<M, crate::Error>
where
    N: serde::Serialize,
    M: serde::de::DeserializeOwned,
{
    let json = serde_json::to_value(params).map_err(|_| crate::Error::parse_error())?;
    let m = serde_json::from_value(json).map_err(|_| crate::Error::parse_error())?;
    Ok(m)
}

pub fn internal_error(message: impl ToString) -> crate::Error {
    crate::Error::internal_error().with_data(message.to_string())
}

pub fn parse_error(message: impl ToString) -> crate::Error {
    crate::Error::parse_error().with_data(message.to_string())
}
