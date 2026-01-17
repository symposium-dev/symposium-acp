//! Run trait for background tasks that run alongside a connection.
//!
//! Run implementations are composable background tasks that run while a connection is active.
//! They're used for things like MCP tool handlers that need to receive calls through
//! channels and invoke user-provided closures.

use std::future::Future;

use crate::{ConnectionTo, link::JrLink};

/// A background task that runs alongside a connection.
///
/// Run implementations are composed using [`ChainRun`] and run in parallel
/// when the connection is active.
pub trait Run<Link: JrLink>: Send {
    /// Run this task to completion.
    fn run(self, cx: ConnectionTo<Link>)
    -> impl Future<Output = Result<(), crate::Error>> + Send;
}

/// A no-op Run that completes immediately.
#[derive(Default)]
pub struct NullRun;

impl<Link: JrLink> Run<Link> for NullRun {
    async fn run(self, _cx: ConnectionTo<Link>) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Chains two Run implementations to run in parallel.
pub struct ChainRun<A, B> {
    a: A,
    b: B,
}

impl<A, B> ChainRun<A, B> {
    /// Create a new chained Run from two Run implementations.
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<Link: JrLink, A: Run<Link>, B: Run<Link>> Run<Link> for ChainRun<A, B> {
    async fn run(self, cx: ConnectionTo<Link>) -> Result<(), crate::Error> {
        // Box the futures to avoid stack overflow with deeply nested Run chains
        let a_fut = Box::pin(self.a.run(cx.clone()));
        let b_fut = Box::pin(self.b.run(cx.clone()));
        let ((), ()) = futures::future::try_join(a_fut, b_fut).await?;
        Ok(())
    }
}

/// A Run created from a closure via [`with_spawned`](crate::ConnectFrom::with_spawned).
pub struct SpawnedRun<F> {
    task_fn: F,
    location: &'static std::panic::Location<'static>,
}

impl<F> SpawnedRun<F> {
    /// Create a new spawned Run from a closure.
    pub fn new(location: &'static std::panic::Location<'static>, task_fn: F) -> Self {
        Self { task_fn, location }
    }
}

impl<Link, F, Fut> Run<Link> for SpawnedRun<F>
where
    Link: JrLink,
    F: FnOnce(ConnectionTo<Link>) -> Fut + Send,
    Fut: Future<Output = Result<(), crate::Error>> + Send,
{
    async fn run(self, cx: ConnectionTo<Link>) -> Result<(), crate::Error> {
        let location = self.location;
        (self.task_fn)(cx).await.map_err(|err| {
            let data = err.data.clone();
            err.data(serde_json::json! {
                {
                    "spawned_at": format!("{}:{}:{}", location.file(), location.line(), location.column()),
                    "data": data,
                }
            })
        })
    }
}
