//! Responder trait for background tasks that run alongside a connection.
//!
//! Responders are composable background tasks that run while a connection is active.
//! They're used for things like MCP tool handlers that need to receive calls through
//! channels and invoke user-provided closures.

use std::future::Future;

/// A responder runs background tasks alongside a connection.
///
/// Responders are composed using [`ChainResponder`] and run in parallel
/// when the connection is active.
pub trait JrResponder: Send {
    /// Run this responder to completion.
    fn run(self) -> impl Future<Output = Result<(), crate::Error>> + Send;
}

/// A no-op responder that completes immediately.
#[derive(Default)]
pub struct NullResponder;

impl JrResponder for NullResponder {
    async fn run(self) -> Result<(), crate::Error> {
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

impl<A: JrResponder, B: JrResponder> JrResponder for ChainResponder<A, B> {
    async fn run(self) -> Result<(), crate::Error> {
        let ((), ()) = futures::future::try_join(self.a.run(), self.b.run()).await?;
        Ok(())
    }
}
