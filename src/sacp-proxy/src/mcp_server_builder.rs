use std::{pin::pin, sync::Arc};

use futures::future::Either;
use fxhash::FxHashMap;
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::tool::cached_schema_for_type,
    model::{CallToolResult, ListToolsResult, Tool},
};
use sacp::{BoxFuture, ByteStreams, Component};

mod tool;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
pub use tool::*;

use crate::McpContext;

/// Our MCP server implementation.
#[derive(Clone, Default)]
pub struct McpServer {
    instructions: Option<String>,
    tool_models: Vec<rmcp::model::Tool>,
    tools: FxHashMap<String, Arc<dyn ErasedMcpTool>>,
}

impl McpServer {
    /// Create an empty server with no content.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the server instructions that are provided to the client.
    pub fn instructions(mut self, instructions: impl ToString) -> Self {
        self.instructions = Some(instructions.to_string());
        self
    }

    /// Add a tool to the server.
    pub fn tool(mut self, tool: impl McpTool + 'static) -> Self {
        let tool_model = make_tool_model(&tool);
        self.tool_models.push(tool_model);
        self.tools.insert(tool.name(), make_erased_mcp_tool(tool));
        self
    }

    /// Create a connection to communicate with this server given the MCP context.
    /// This is pub(crate) because it is only used internally by the MCP server registry.
    pub(crate) fn new_connection(&self, mcp_cx: McpContext) -> McpServerConnection {
        McpServerConnection {
            service: self.clone(),
            mcp_cx,
        }
    }
}

/// An MCP server instance connected to the ACP framework.
pub(crate) struct McpServerConnection {
    service: McpServer,
    mcp_cx: McpContext,
}

impl Component for McpServerConnection {
    async fn serve(self, client: impl Component) -> Result<(), sacp::Error> {
        // Create tokio byte streams that rmcp expects
        let (mcp_server_stream, mcp_client_stream) = tokio::io::duplex(8192);
        let (mcp_server_read, mcp_server_write) = tokio::io::split(mcp_server_stream);
        let (mcp_client_read, mcp_client_write) = tokio::io::split(mcp_client_stream);

        // Create ByteStreams component for the client side
        let byte_streams =
            ByteStreams::new(mcp_client_write.compat_write(), mcp_client_read.compat());

        // Spawn task to connect byte_streams to the provided client
        tokio::spawn(async move {
            let _ = byte_streams.serve(client).await;
        });

        // Run the rmcp server with the server side of the duplex stream
        let running_server = rmcp::ServiceExt::serve(self, (mcp_server_read, mcp_server_write))
            .await
            .map_err(sacp::Error::into_internal_error)?;

        // Wait for the server to finish
        running_server
            .waiting()
            .await
            .map(|_quit_reason| ())
            .map_err(sacp::Error::into_internal_error)
    }
}

impl ServerHandler for McpServerConnection {
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParam,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        // Lookup the tool definition, erroring if not found
        let Some(tool) = self.service.tools.get(&request.name[..]) else {
            return Err(rmcp::model::ErrorData::invalid_params(
                format!("tool `{}` not found", request.name),
                None,
            ));
        };

        // Convert input into JSON
        let serde_value = serde_json::to_value(request.arguments).expect("valid json");

        // Execute the user's tool, unless cancellation occurs
        match futures::future::select(
            tool.call_tool(serde_value, self.mcp_cx.clone()),
            pin!(context.ct.cancelled()),
        )
        .await
        {
            // If completed successfully
            Either::Left((m, _)) => match m {
                Ok(result) => Ok(CallToolResult::structured(result)),
                Err(error) => Err(to_rmcp_error(error)),
            },

            // If cancelled
            Either::Right(((), _)) => {
                Err(rmcp::ErrorData::internal_error("operation cancelled", None))
            }
        }
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
        // Just return all tools
        Ok(ListToolsResult::with_all_items(
            self.service.tool_models.clone(),
        ))
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        // Basic server info
        rmcp::model::ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::default(),
            capabilities: rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: rmcp::model::Implementation::default(),
            instructions: self.service.instructions.clone(),
        }
    }
}

/// Erased version of the MCP tool trait that is dyn-compatible.
trait ErasedMcpTool: Send + Sync {
    fn call_tool(
        &self,
        input: serde_json::Value,
        context: McpContext,
    ) -> BoxFuture<'_, Result<serde_json::Value, sacp::Error>>;
}

//// Create an `rmcp` tool mode from our [`McpTool`] trait.
fn make_tool_model<M: McpTool>(tool: &M) -> Tool {
    rmcp::model::Tool {
        name: tool.name().into(),
        title: tool.title(),
        description: Some(tool.description().into()),
        input_schema: cached_schema_for_type::<M::Input>(),
        output_schema: Some(cached_schema_for_type::<M::Output>()),
        annotations: None,
        icons: None,
    }
}

/// Create a [`ErasedMcpTool`] from a [`McpTool`], erasing the type details.
fn make_erased_mcp_tool<'s, M: McpTool + 's>(tool: M) -> Arc<dyn ErasedMcpTool + 's> {
    struct ErasedMcpToolImpl<M: McpTool> {
        tool: M,
    }

    impl<M: McpTool> ErasedMcpTool for ErasedMcpToolImpl<M> {
        fn call_tool(
            &self,
            input: serde_json::Value,
            context: McpContext,
        ) -> BoxFuture<'_, Result<serde_json::Value, sacp::Error>> {
            Box::pin(async move {
                let input = serde_json::from_value(input).map_err(sacp::util::internal_error)?;
                serde_json::to_value(self.tool.call_tool(input, context).await?)
                    .map_err(sacp::util::internal_error)
            })
        }
    }

    Arc::new(ErasedMcpToolImpl { tool })
}

/// Convert a [`sacp::Error`] into an [`rmcp::ErrorData`].
fn to_rmcp_error(error: sacp::Error) -> rmcp::ErrorData {
    rmcp::ErrorData {
        code: rmcp::model::ErrorCode(error.code),
        message: error.message.into(),
        data: error.data,
    }
}
