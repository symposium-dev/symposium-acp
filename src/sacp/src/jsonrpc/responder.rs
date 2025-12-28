//! Responder trait for background tasks that run alongside a connection.
//!
//! Responders are composable background tasks that run while a connection is active.
//! They're used for things like MCP tool handlers that need to receive calls through
//! channels and invoke user-provided closures.

use std::future::Future;

use crate::{JrConnectionCx, link::JrLink};

/// A responder runs background tasks alongside a connection.
///
/// Responders are composed using [`ChainResponder`] and run in parallel
/// when the connection is active.
pub trait JrResponder<Link: JrLink>: Send {
    /// Run this responder to completion.
    fn run(self, cx: JrConnectionCx<Link>)
    -> impl Future<Output = Result<(), crate::Error>> + Send;
}

/// A no-op responder that completes immediately.
#[derive(Default)]
pub struct NullResponder;

impl<Link: JrLink> JrResponder<Link> for NullResponder {
    async fn run(self, _cx: JrConnectionCx<Link>) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Chains two responders to run in parallel.
pub struct ChainResponder<A, B> {
    a: A,
    b: B,
}

impl<A, B> ChainResponder<A, B> {
    /// Create a new chained responder from two responders.
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<Link: JrLink, A: JrResponder<Link>, B: JrResponder<Link>> JrResponder<Link>
    for ChainResponder<A, B>
{
    async fn run(self, cx: JrConnectionCx<Link>) -> Result<(), crate::Error> {
        // Box the futures to avoid stack overflow with deeply nested responder chains
        let a_fut = Box::pin(self.a.run(cx.clone()));
        let b_fut = Box::pin(self.b.run(cx.clone()));
        let ((), ()) = futures::future::try_join(a_fut, b_fut).await?;
        Ok(())
    }
}

/// A responder created from a closure via [`with_spawned`](crate::JrConnectionBuilder::with_spawned).
pub struct SpawnedResponder<F> {
    task_fn: F,
    location: &'static std::panic::Location<'static>,
}

impl<F> SpawnedResponder<F> {
    /// Create a new spawned responder from a closure.
    pub fn new(location: &'static std::panic::Location<'static>, task_fn: F) -> Self {
        Self { task_fn, location }
    }
}

impl<Link, F, Fut> JrResponder<Link> for SpawnedResponder<F>
where
    Link: JrLink,
    F: FnOnce(JrConnectionCx<Link>) -> Fut + Send,
    Fut: Future<Output = Result<(), crate::Error>> + Send,
{
    async fn run(self, cx: JrConnectionCx<Link>) -> Result<(), crate::Error> {
        let location = self.location;
        (self.task_fn)(cx).await.map_err(|err| {
            let data = err.data.clone();
            err.with_data(serde_json::json! {
                {
                    "spawned_at": format!("{}:{}:{}", location.file(), location.line(), location.column()),
                    "data": data,
                }
            })
        })
    }
}
