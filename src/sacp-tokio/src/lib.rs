//! Tokio-based utilities for SACP
//!
//! This crate provides higher-level functionality for working with SACP
//! that requires the Tokio async runtime, such as spawning agent processes
//! and creating connections.

mod acp_agent;

pub use acp_agent::AcpAgent;
use sacp::{ByteStreams, IntoJrTransport};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Default)]
pub struct Stdio {
    _private: (),
}

impl IntoJrTransport for Stdio {
    fn into_jr_transport(
        self: Box<Self>,
        cx: &sacp::JrConnectionCx,
        outgoing_rx: futures::channel::mpsc::UnboundedReceiver<jsonrpcmsg::Message>,
        incoming_tx: futures::channel::mpsc::UnboundedSender<jsonrpcmsg::Message>,
    ) -> Result<(), sacp::Error> {
        Box::new(ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .into_jr_transport(cx, outgoing_rx, incoming_tx)
    }
}
