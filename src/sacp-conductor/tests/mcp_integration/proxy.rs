//! Proxy component that provides MCP tools

use sacp::{BoxFuture, Channels, Component, JrHandlerChain};
use sacp_proxy::{AcpProxyExt, McpServiceRegistry};

use crate::mcp_integration::mcp_server::TestMcpServer;

pub struct ProxyComponent;

impl Component for ProxyComponent {
    fn serve(self: Box<Self>, channels: Channels) -> BoxFuture<'static, Result<(), sacp::Error>> {
        Box::pin(async move {
            JrHandlerChain::new()
                .name("proxy-component")
                .provide_mcp(
                    McpServiceRegistry::default().with_rmcp_server("test", TestMcpServer::new)?,
                )
                .proxy()
                .serve(channels)
                .await
        })
    }
}

pub fn create() -> Box<dyn Component> {
    Box::new(ProxyComponent)
}
