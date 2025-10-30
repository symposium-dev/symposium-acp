//! Proxy component that provides MCP tools

use futures::{AsyncRead, AsyncWrite};
use sacp::{JrConnection, JrConnectionCx};
use sacp_conductor::component::{Cleanup, ComponentProvider};
use sacp_proxy::{AcpProxyExt, McpServiceRegistry};
use std::pin::Pin;

use crate::mcp_integration::mcp_server::TestMcpServer;

pub struct ProxyComponentProvider;

impl ComponentProvider for ProxyComponentProvider {
    fn create(
        &self,
        cx: &JrConnectionCx,
        outgoing_bytes: Pin<Box<dyn AsyncWrite + Send>>,
        incoming_bytes: Pin<Box<dyn AsyncRead + Send>>,
    ) -> Result<Cleanup, sacp::Error> {
        cx.spawn(
            JrConnection::new(outgoing_bytes, incoming_bytes)
                .name("proxy-component")
                .provide_mcp(
                    McpServiceRegistry::default().with_rmcp_server("test", TestMcpServer::new)?,
                )
                .proxy()
                .serve(),
        )?;

        Ok(Cleanup::None)
    }
}

pub fn create() -> Box<dyn ComponentProvider> {
    Box::new(ProxyComponentProvider)
}
