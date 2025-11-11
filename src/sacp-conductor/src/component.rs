use sacp;
use sacp_tokio::AcpAgent;
use std::pin::Pin;
use tokio_util::compat::{
    FuturesAsyncReadCompatExt as _, FuturesAsyncWriteCompatExt, TokioAsyncReadCompatExt as _,
    TokioAsyncWriteCompatExt as _,
};

use futures::channel::mpsc;
use futures::{AsyncRead, AsyncWrite};

use sacp::{IntoJrTransport, JrConnectionCx};
use tokio::process::Child;
use tracing::debug;

/// "Provider" used to spawn components. The `CommandProvider` is used from the CLI, but for testing
/// or internal purposes, other component providers may be created.
pub trait ComponentProvider: Send {
    /// Create a component that will read/write ACP messages from the given streams.
    /// The `cx` can be used to spawn tasks running in the JSON RPC connection.
    ///
    /// # Parameters
    ///
    /// * `cx`: The JSON RPC connection context.
    /// * `outgoing_bytes`: bytes sent from the component to the conductor.
    /// * `incoming_bytes`: bytes received by the conponent from the conductor.
    fn create(
        &self,
        cx: &JrConnectionCx,
        outgoing_bytes: Pin<Box<dyn AsyncWrite + Send>>,
        incoming_bytes: Pin<Box<dyn AsyncRead + Send>>,
    ) -> Result<Cleanup, sacp::Error>;
}

/// Cleanup enum returned by component provider.
/// Will be dropped when server executed.
#[non_exhaustive]
pub enum Cleanup {
    /// No cleanup required.
    None,
    /// Send "kill" signal to the process.
    KillProcess(Child),
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        match self {
            Cleanup::None => {}
            Cleanup::KillProcess(child) => {
                let _: Result<_, _> = child.start_kill();
            }
        }
    }
}

/// A spawned component in the proxy chain.
///
/// This represents a component that has been launched and is connected
/// to the conductor via JSON-RPC.
pub struct Component {
    /// The child process, if this component was spawned via Command.
    /// This is used to kill the child process when the component is dropped.
    /// None for mock components used in tests.
    pub cleanup: Cleanup,

    /// The connection context to the component. This is called `agent_cx` because the
    /// component is acting as the conductor's agent.
    pub agent_cx: JrConnectionCx,
}

/// A "command provider" provides a component by running a command and sending ACP messages to/from stdio.
pub struct CommandComponentProvider {
    command: String,
}

impl CommandComponentProvider {
    pub fn new(command: String) -> Box<dyn ComponentProvider> {
        Box::new(CommandComponentProvider { command })
    }
}

impl ComponentProvider for CommandComponentProvider {
    fn create(
        &self,
        cx: &JrConnectionCx,
        outgoing_bytes: Pin<Box<dyn AsyncWrite + Send>>,
        incoming_bytes: Pin<Box<dyn AsyncRead + Send>>,
    ) -> Result<Cleanup, sacp::Error> {
        debug!(command = self.command, "Spawning command");

        // Parse the command string into program and arguments
        let parts = shell_words::split(&self.command)
            .map_err(|e| sacp::util::internal_error(format!("Failed to parse command: {}", e)))?;

        if parts.is_empty() {
            return Err(sacp::util::internal_error("Command string cannot be empty"));
        }

        let mut child = tokio::process::Command::new(&parts[0])
            .args(&parts[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(sacp::Error::into_internal_error)?;

        // Take ownership of the streams (can only do this once!)
        let mut child_stdin = child.stdin.take().expect("Failed to open stdin");
        let mut child_stdout = child.stdout.take().expect("Failed to open stdout");

        cx.spawn(async move {
            tokio::io::copy(&mut incoming_bytes.compat(), &mut child_stdin)
                .await
                .map_err(sacp::Error::into_internal_error)?;
            Ok(())
        })?;

        cx.spawn(async move {
            tokio::io::copy(&mut child_stdout, &mut outgoing_bytes.compat_write())
                .await
                .map_err(sacp::Error::into_internal_error)?;
            Ok(())
        })?;

        Ok(Cleanup::KillProcess(child))
    }
}

impl IntoJrTransport for Box<dyn ComponentProvider> {
    fn into_jr_transport(
        self: Box<Self>,
        cx: &JrConnectionCx,
        outgoing_rx: mpsc::UnboundedReceiver<sacp::JsonRpcMessage>,
        incoming_tx: mpsc::UnboundedSender<sacp::JsonRpcMessage>,
    ) -> Result<(), sacp::Error> {
        use tokio::io::duplex;

        // Create byte streams using tokio duplex channels
        let (outgoing_write, outgoing_read) = duplex(8192);
        let (incoming_write, incoming_read) = duplex(8192);

        // Create the component with the byte streams
        let _cleanup = self.create(
            cx,
            Box::pin(outgoing_write.compat_write()),
            Box::pin(incoming_read.compat()),
        )?;

        // Delegate to ByteStreams for the transport layer
        Box::new(sacp::ByteStreams::new(
            incoming_write.compat_write(),
            outgoing_read.compat(),
        ))
        .into_jr_transport(cx, outgoing_rx, incoming_tx)
    }
}

impl ComponentProvider for AcpAgent {
    fn create(
        &self,
        cx: &JrConnectionCx,
        outgoing_bytes: Pin<Box<dyn AsyncWrite + Send>>,
        incoming_bytes: Pin<Box<dyn AsyncRead + Send>>,
    ) -> Result<Cleanup, sacp::Error> {
        debug!(agent = ?self, "Spawning AcpAgent");

        let (mut child_stdin, mut child_stdout, child) = self.spawn_process()?;

        cx.spawn(async move {
            tokio::io::copy(&mut incoming_bytes.compat(), &mut child_stdin)
                .await
                .map_err(sacp::Error::into_internal_error)?;
            Ok(())
        })?;

        cx.spawn(async move {
            tokio::io::copy(&mut child_stdout, &mut outgoing_bytes.compat_write())
                .await
                .map_err(sacp::Error::into_internal_error)?;
            Ok(())
        })?;

        Ok(Cleanup::KillProcess(child))
    }
}
