//! MCP server builder for creating MCP servers.

use std::{pin::pin, sync::Arc};

use futures::{
    SinkExt,
    channel::{mpsc, oneshot},
    future::{BoxFuture, Either},
};
use fxhash::FxHashMap;
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::tool::cached_schema_for_type,
    model::{CallToolResult, ListToolsResult, Tool},
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{McpContext, McpTool};
use crate::{
    Agent, ByteStreams, Component, DynComponent, HasEndpoint, JrRole,
    jsonrpc::responder::{ChainResponder, JrResponder, NullResponder},
    mcp_server::{
        McpServer, McpServerConnect,
        responder::{ToolCall, ToolFnResponder},
    },
};

/// Builder for creating MCP servers with tools.
///
/// Use [`McpServer::builder`] to create a new builder, then chain methods to
/// configure the server and call [`build`](Self::build) to create the server.
///
/// # Example
///
/// ```rust,ignore
/// let server = McpServer::builder("my-server".to_string())
///     .instructions("A helpful assistant")
///     .tool(EchoTool)
///     .tool_fn(
///         "greet",
///         "Greet someone by name",
///         async |input: GreetInput, _cx| Ok(format!("Hello, {}!", input.name)),
///         sacp::tool_fn!(),
///     )
///     .build();
/// ```
pub struct McpServerBuilder<Role: JrRole, Responder: JrResponder = NullResponder>
where
    Role: HasEndpoint<Agent>,
{
    role: Role,
    name: String,
    data: McpServerData<Role>,
    responder: Responder,
}

#[derive(Default)]
struct McpServerData<Role: JrRole> {
    instructions: Option<String>,
    tool_models: Vec<rmcp::model::Tool>,
    tools: FxHashMap<String, Arc<dyn ErasedMcpTool<Role>>>,
}

impl<Role: JrRole> McpServerBuilder<Role, NullResponder>
where
    Role: HasEndpoint<Agent>,
{
    pub(super) fn new(name: String) -> Self {
        Self {
            name: name,
            role: Role::default(),
            data: McpServerData::default(),
            responder: NullResponder::default(),
        }
    }
}

impl<Role: JrRole, Responder: JrResponder> McpServerBuilder<Role, Responder>
where
    Role: HasEndpoint<Agent>,
{
    /// Set the server instructions that are provided to the client.
    pub fn instructions(mut self, instructions: impl ToString) -> Self {
        self.data.instructions = Some(instructions.to_string());
        self
    }

    /// Add a tool to the server.
    pub fn tool(mut self, tool: impl McpTool<Role> + 'static) -> Self {
        let tool_model = make_tool_model(&tool);
        self.data.tool_models.push(tool_model);
        self.data
            .tools
            .insert(tool.name(), make_erased_mcp_tool(tool));
        self
    }

    /// Private fn: adds the tool but also adds a responder that will be
    /// run while the MCP server is active.
    fn tool_with_responder<R: JrResponder>(
        self,
        tool: impl McpTool<Role> + 'static,
        tool_responder: R,
    ) -> McpServerBuilder<Role, ChainResponder<Responder, R>> {
        let this = self.tool(tool);
        McpServerBuilder {
            role: this.role,
            name: this.name,
            data: this.data,
            responder: ChainResponder::new(this.responder, tool_responder),
        }
    }

    /// Convenience wrapper for defining a tool without having to create a struct.
    ///
    /// # Parameters
    ///
    /// * `name`: The name of the tool.
    /// * `description`: The description of the tool.
    /// * `func`: The function that implements the tool. Use an async closure like `async |args, cx| { .. }`.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// McpServer::builder("my-server")
    ///     .tool_fn(
    ///         "greet",
    ///         "Greet someone by name",
    ///         async |input: GreetInput, _cx| Ok(format!("Hello, {}!", input.name)),
    ///     )
    /// ```
    pub fn tool_fn<P, R, F, Fut>(
        self,
        name: impl ToString,
        description: impl ToString,
        func: F,
    ) -> McpServerBuilder<Role, ChainResponder<Responder, ToolFnResponder<F, P, R, Role>>>
    where
        P: JsonSchema + DeserializeOwned + 'static + Send,
        R: JsonSchema + Serialize + 'static + Send,
        F: FnMut(P, McpContext<Role>) -> Fut + Send + Sync,
        Fut: std::future::Future<Output = Result<R, crate::Error>> + Send,
    {
        struct ToolFnTool<P, R, Role: JrRole> {
            name: String,
            description: String,
            call_tx: mpsc::Sender<ToolCall<P, R, Role>>,
        }

        impl<P, R, Role> McpTool<Role> for ToolFnTool<P, R, Role>
        where
            Role: JrRole,
            P: JsonSchema + DeserializeOwned + 'static + Send,
            R: JsonSchema + Serialize + 'static + Send,
        {
            type Input = P;
            type Output = R;

            fn name(&self) -> String {
                self.name.clone()
            }

            fn description(&self) -> String {
                self.description.clone()
            }

            async fn call_tool(
                &self,
                params: P,
                mcp_cx: McpContext<Role>,
            ) -> Result<R, crate::Error> {
                let (result_tx, result_rx) = oneshot::channel();

                self.call_tx
                    .clone()
                    .send(ToolCall {
                        params,
                        mcp_cx,
                        result_tx,
                    })
                    .await
                    .map_err(crate::util::internal_error)?;

                result_rx.await.map_err(crate::util::internal_error)?
            }
        }

        let (call_tx, call_rx) = mpsc::channel(128);
        self.tool_with_responder(
            ToolFnTool {
                name: name.to_string(),
                description: description.to_string(),
                call_tx,
            },
            ToolFnResponder { func, call_rx },
        )
    }

    /// Create an MCP server from this builder.
    ///
    /// This builder can be attached to new sessions (see [`SessionBuilder::with_mcp_server`])
    /// or served up as part of a proxy (see [`JrConnectionBuilder::with_mcp_server`]).
    pub fn build(self) -> McpServer<Role, Responder> {
        McpServer::new(
            McpServerBuilt {
                role: self.role,
                name: self.name,
                data: Arc::new(self.data),
            },
            self.responder,
        )
    }
}

struct McpServerBuilt<Role: JrRole>
where
    Role: HasEndpoint<Agent>,
{
    #[expect(dead_code)]
    role: Role,
    name: String,
    data: Arc<McpServerData<Role>>,
}

impl<'scope, Role: JrRole> McpServerConnect<Role> for McpServerBuilt<Role>
where
    Role: HasEndpoint<Agent>,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    fn connect(&self, mcp_cx: McpContext<Role>) -> DynComponent {
        DynComponent::new(McpServerConnection {
            data: self.data.clone(),
            mcp_cx,
        })
    }
}

/// An MCP server instance connected to the ACP framework.
pub(crate) struct McpServerConnection<Role: JrRole>
where
    Role: HasEndpoint<Agent>,
{
    data: Arc<McpServerData<Role>>,
    mcp_cx: McpContext<Role>,
}

impl<Role: JrRole> Component for McpServerConnection<Role>
where
    Role: HasEndpoint<Agent>,
{
    async fn serve(self, client: impl Component) -> Result<(), crate::Error> {
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
            .map_err(crate::Error::into_internal_error)?;

        // Wait for the server to finish
        running_server
            .waiting()
            .await
            .map(|_quit_reason| ())
            .map_err(crate::Error::into_internal_error)
    }
}

impl<Role: JrRole> ServerHandler for McpServerConnection<Role>
where
    Role: HasEndpoint<Agent>,
{
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParam,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        // Lookup the tool definition, erroring if not found
        let Some(tool) = self.data.tools.get(&request.name[..]) else {
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
            self.data.tool_models.clone(),
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
            instructions: self.data.instructions.clone(),
        }
    }
}

/// Erased version of the MCP tool trait that is dyn-compatible.
trait ErasedMcpTool<Role: JrRole>: Send + Sync {
    fn call_tool(
        &self,
        input: serde_json::Value,
        context: McpContext<Role>,
    ) -> BoxFuture<'_, Result<serde_json::Value, crate::Error>>;
}

/// Create an `rmcp` tool model from our [`McpTool`] trait.
fn make_tool_model<Role: JrRole, M: McpTool<Role>>(tool: &M) -> Tool {
    rmcp::model::Tool {
        name: tool.name().into(),
        title: tool.title(),
        description: Some(tool.description().into()),
        input_schema: cached_schema_for_type::<M::Input>(),
        output_schema: Some(cached_schema_for_type::<M::Output>()),
        annotations: None,
        icons: None,
        meta: None,
    }
}

/// Create a [`ErasedMcpTool`] from a [`McpTool`], erasing the type details.
fn make_erased_mcp_tool<'s, Role: JrRole, M: McpTool<Role> + 's>(
    tool: M,
) -> Arc<dyn ErasedMcpTool<Role> + 's> {
    struct ErasedMcpToolImpl<M> {
        tool: M,
    }

    impl<Role, M> ErasedMcpTool<Role> for ErasedMcpToolImpl<M>
    where
        Role: JrRole,
        M: McpTool<Role>,
    {
        fn call_tool(
            &self,
            input: serde_json::Value,
            context: McpContext<Role>,
        ) -> BoxFuture<'_, Result<serde_json::Value, crate::Error>> {
            Box::pin(async move {
                let input = serde_json::from_value(input).map_err(crate::util::internal_error)?;
                serde_json::to_value(self.tool.call_tool(input, context).await?)
                    .map_err(crate::util::internal_error)
            })
        }
    }

    Arc::new(ErasedMcpToolImpl { tool })
}

/// Convert a [`crate::Error`] into an [`rmcp::ErrorData`].
fn to_rmcp_error(error: crate::Error) -> rmcp::ErrorData {
    rmcp::ErrorData {
        code: rmcp::model::ErrorCode(error.code),
        message: error.message.into(),
        data: error.data,
    }
}
