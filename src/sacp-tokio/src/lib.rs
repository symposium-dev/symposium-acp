//! Tokio-based utilities for SACP
//!
//! This crate provides higher-level functionality for working with SACP
//! that requires the Tokio async runtime, such as spawning agent processes
//! and creating connections.

mod acp_agent;

pub use acp_agent::AcpAgent;
use sacp::{ByteStreams, Transport};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Default)]
pub struct Stdio {
    _private: (),
}

impl Transport for Stdio {
    fn transport(
        self: Box<Self>,
        channels: sacp::Channels,
    ) -> sacp::BoxFuture<'static, Result<(), sacp::Error>> {
        Box::new(ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .transport(channels)
    }
}
