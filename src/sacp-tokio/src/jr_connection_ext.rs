//! Extension utilities for `JrConnection` to support spawning agents with Tokio.

use crate::AcpAgent;
use sacp::JrConnection;
use sacp::handler::NullHandler;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::process::Child;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// A future that holds a `Child` process and never resolves.
/// When dropped, the child process is killed.
struct ChildHolder {
    _child: Child,
}

impl Future for ChildHolder {
    type Output = Result<(), sacp::Error>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Never ready - just hold the child process alive
        Poll::Pending
    }
}

impl Drop for ChildHolder {
    fn drop(&mut self) {
        let _: Result<_, _> = self._child.start_kill();
    }
}

pub trait JrConnectionExt {
    /// Spawn an agent and create a connection to it.
    ///
    /// The child process is managed automatically - a background task holds it alive,
    /// and when the connection is dropped, the process will be killed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use sacp::JrConnection;
    /// # use std::str::FromStr;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # use sacp_tokio::AcpAgent;
    /// use sacp_tokio::JrConnectionExt;
    /// let agent = AcpAgent::from_str("python my_agent.py")?;
    ///
    /// JrConnection::to_agent(agent)?
    ///     .on_receive_notification(|notif: sacp::SessionNotification, _cx| async move {
    ///         println!("{:?}", notif);
    ///         Ok(())
    ///     })
    ///     .with_client(|cx: sacp::JrConnectionCx| async move {
    ///         // Use the connection...
    ///         Ok(())
    ///     })
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    fn to_agent(
        agent: AcpAgent,
    ) -> Result<
        sacp::JrConnection<
            impl futures::AsyncWrite + Send + 'static,
            impl futures::AsyncRead + Send + 'static,
            NullHandler,
        >,
        sacp::Error,
    >;
}

impl JrConnectionExt
    for JrConnection<
        Pin<Box<dyn futures::AsyncWrite>>,
        Pin<Box<dyn futures::AsyncRead>>,
        NullHandler,
    >
{
    fn to_agent(
        agent: AcpAgent,
    ) -> Result<
        JrConnection<
            impl futures::AsyncWrite + Send + 'static,
            impl futures::AsyncRead + Send + 'static,
            NullHandler,
        >,
        sacp::Error,
    > {
        let (child_stdin, child_stdout, child) = agent.spawn_process()?;

        let connection = sacp::JrConnection::new(child_stdin.compat_write(), child_stdout.compat())
            .with_spawned(ChildHolder { _child: child });

        Ok(connection)
    }
}
