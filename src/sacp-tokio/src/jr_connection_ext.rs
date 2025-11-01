//! Extension utilities for `JrConnection` to support spawning agents with Tokio.

use crate::AcpAgent;
use sacp::{JrConnection, NullHandler};
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

/// Spawn an agent and create a connection to it.
///
/// The child process is managed automatically - a background task holds it alive,
/// and when the connection is dropped, the process will be killed.
///
/// # Example
///
/// ```no_run
/// # use sacp_tokio::{AcpAgent, to_agent};
/// # use std::str::FromStr;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let agent = AcpAgent::from_str("python my_agent.py")?;
///
/// to_agent(agent)?
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
pub fn to_agent(
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

    let connection = JrConnection::new(child_stdin.compat_write(), child_stdout.compat())
        .with_spawned(ChildHolder { _child: child });

    Ok(connection)
}
