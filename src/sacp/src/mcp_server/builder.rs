//! MCP server builder for creating MCP servers.

use std::{collections::HashSet, pin::pin, sync::Arc};

use futures::{
    SinkExt,
    channel::{mpsc, oneshot},
    future::{BoxFuture, Either},
};
use fxhash::FxHashMap;

/// Tracks which tools are enabled.
///
/// - `DenyList`: All tools enabled except those in the set (default)
/// - `AllowList`: Only tools in the set are enabled
#[derive(Clone, Debug)]
pub enum EnabledTools {
    /// All tools enabled except those in the deny set.
    DenyList(HashSet<String>),
    /// Only tools in the allow set are enabled.
    AllowList(HashSet<String>),
}

impl Default for EnabledTools {
    fn default() -> Self {
        EnabledTools::DenyList(HashSet::new())
    }
}

impl EnabledTools {
    /// Check if a tool is enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        match self {
            EnabledTools::DenyList(deny) => !deny.contains(name),
            EnabledTools::AllowList(allow) => allow.contains(name),
        }
    }
}
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::tool::{schema_for_output, schema_for_type},
    model::{CallToolResult, ListToolsResult, Tool},
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{McpContext, McpTool};
use crate::{
    ByteStreams, Component, DynComponent, JrLink,
    jsonrpc::responder::{ChainResponder, JrResponder, NullResponder},
    mcp_server::{
        McpServer, McpServerConnect,
        responder::{ToolCall, ToolFnMutResponder, ToolFnResponder},
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
pub struct McpServerBuilder<Link: JrLink, Responder> {
    role: Link,
    name: String,
    data: McpServerData<Link>,
    responder: Responder,
}

struct McpServerData<Link: JrLink> {
    instructions: Option<String>,
    tool_models: Vec<rmcp::model::Tool>,
    tools: FxHashMap<String, RegisteredTool<Link>>,
    enabled_tools: EnabledTools,
}

/// A registered tool with its metadata.
struct RegisteredTool<Link: JrLink> {
    tool: Arc<dyn ErasedMcpTool<Link>>,
    /// Whether this tool returns structured output (i.e., has an output_schema).
    has_structured_output: bool,
}

impl<Link: JrLink> Default for McpServerData<Link> {
    fn default() -> Self {
        Self {
            instructions: None,
            tool_models: Vec::new(),
            tools: FxHashMap::default(),
            enabled_tools: EnabledTools::default(),
        }
    }
}

impl<Link: JrLink> McpServerBuilder<Link, NullResponder> {
    pub(super) fn new(name: String) -> Self {
        Self {
            name: name,
            role: Link::default(),
            data: McpServerData::default(),
            responder: NullResponder::default(),
        }
    }
}

impl<Link: JrLink, Responder: JrResponder<Link>> McpServerBuilder<Link, Responder> {
    /// Set the server instructions that are provided to the client.
    pub fn instructions(mut self, instructions: impl ToString) -> Self {
        self.data.instructions = Some(instructions.to_string());
        self
    }

    /// Add a tool to the server.
    pub fn tool(mut self, tool: impl McpTool<Link> + 'static) -> Self {
        let tool_model = make_tool_model(&tool);
        let has_structured_output = tool_model.output_schema.is_some();
        self.data.tool_models.push(tool_model);
        self.data.tools.insert(
            tool.name(),
            RegisteredTool {
                tool: make_erased_mcp_tool(tool),
                has_structured_output,
            },
        );
        self
    }

    /// Disable all tools. After calling this, only tools explicitly enabled
    /// with [`enable_tool`](Self::enable_tool) will be available.
    pub fn disable_all_tools(mut self) -> Self {
        self.data.enabled_tools = EnabledTools::AllowList(HashSet::new());
        self
    }

    /// Enable all tools. After calling this, all tools will be available
    /// except those explicitly disabled with [`disable_tool`](Self::disable_tool).
    pub fn enable_all_tools(mut self) -> Self {
        self.data.enabled_tools = EnabledTools::DenyList(HashSet::new());
        self
    }

    /// Disable a specific tool by name.
    ///
    /// Returns an error if the tool is not registered.
    pub fn disable_tool(mut self, name: &str) -> Result<Self, crate::Error> {
        if !self.data.tools.contains_key(name) {
            return Err(
                crate::Error::invalid_request().with_data(format!("unknown tool: {}", name))
            );
        }
        match &mut self.data.enabled_tools {
            EnabledTools::DenyList(deny) => {
                deny.insert(name.to_string());
            }
            EnabledTools::AllowList(allow) => {
                allow.remove(name);
            }
        }
        Ok(self)
    }

    /// Enable a specific tool by name.
    ///
    /// Returns an error if the tool is not registered.
    pub fn enable_tool(mut self, name: &str) -> Result<Self, crate::Error> {
        if !self.data.tools.contains_key(name) {
            return Err(
                crate::Error::invalid_request().with_data(format!("unknown tool: {}", name))
            );
        }
        match &mut self.data.enabled_tools {
            EnabledTools::DenyList(deny) => {
                deny.remove(name);
            }
            EnabledTools::AllowList(allow) => {
                allow.insert(name.to_string());
            }
        }
        Ok(self)
    }

    /// Private fn: adds the tool but also adds a responder that will be
    /// run while the MCP server is active.
    fn tool_with_responder<R: JrResponder<Link>>(
        self,
        tool: impl McpTool<Link> + 'static,
        tool_responder: R,
    ) -> McpServerBuilder<Link, impl JrResponder<Link>> {
        let this = self.tool(tool);
        McpServerBuilder {
            role: this.role,
            name: this.name,
            data: this.data,
            responder: ChainResponder::new(this.responder, tool_responder),
        }
    }

    /// Convenience wrapper for defining a "single-threaded" tool without having to create a struct.
    /// By "single-threaded", we mean that only one invocation of the tool can be running at a time.
    /// Typically agents invoke a tool once per session and then block waiting for the result,
    /// so this is fine, but they could attempt to run multiple invocations concurrently, in which
    /// case those invocations would be serialized.
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
    ///     .tool_fn_mut(
    ///         "greet",
    ///         "Greet someone by name",
    ///         async |input: GreetInput, _cx| Ok(format!("Hello, {}!", input.name)),
    ///     )
    /// ```
    pub fn tool_fn_mut<P, R, F>(
        self,
        name: impl ToString,
        description: impl ToString,
        func: F,
        tool_future_hack: impl for<'a> Fn(
            &'a mut F,
            P,
            McpContext<Link>,
        ) -> BoxFuture<'a, Result<R, crate::Error>>
        + Send
        + 'static,
    ) -> McpServerBuilder<Link, impl JrResponder<Link>>
    where
        P: JsonSchema + DeserializeOwned + 'static + Send,
        R: JsonSchema + Serialize + 'static + Send,
        F: AsyncFnMut(P, McpContext<Link>) -> Result<R, crate::Error> + Send,
    {
        let (call_tx, call_rx) = mpsc::channel(128);
        self.tool_with_responder(
            ToolFnTool {
                name: name.to_string(),
                description: description.to_string(),
                call_tx,
            },
            ToolFnMutResponder {
                func,
                call_rx,
                tool_future_fn: Box::new(tool_future_hack),
            },
        )
    }

    /// Convenience wrapper for defining a stateless tool that can run concurrently.
    /// Unlike [`tool_fn_mut`](Self::tool_fn_mut), multiple invocations of this tool can run
    /// at the same time since the function is `Fn` rather than `FnMut`.
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
    pub fn tool_fn<P, R, F>(
        self,
        name: impl ToString,
        description: impl ToString,
        func: F,
        tool_future_hack: impl for<'a> Fn(
            &'a F,
            P,
            McpContext<Link>,
        ) -> BoxFuture<'a, Result<R, crate::Error>>
        + Send
        + Sync
        + 'static,
    ) -> McpServerBuilder<Link, impl JrResponder<Link>>
    where
        P: JsonSchema + DeserializeOwned + 'static + Send,
        R: JsonSchema + Serialize + 'static + Send,
        F: AsyncFn(P, McpContext<Link>) -> Result<R, crate::Error> + Send + Sync + 'static,
    {
        let (call_tx, call_rx) = mpsc::channel(128);
        self.tool_with_responder(
            ToolFnTool {
                name: name.to_string(),
                description: description.to_string(),
                call_tx,
            },
            ToolFnResponder {
                func: func,
                call_rx,
                tool_future_fn: Box::new(tool_future_hack),
            },
        )
    }

    /// Create an MCP server from this builder.
    ///
    /// This builder can be attached to new sessions (see [`SessionBuilder::with_mcp_server`](`crate::SessionBuilder::with_mcp_server`))
    /// or served up as part of a proxy (see [`JrConnectionBuilder::with_mcp_server`](`crate::JrConnectionBuilder::with_mcp_server`)).
    pub fn build(self) -> McpServer<Link, Responder> {
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

struct McpServerBuilt<Link: JrLink> {
    #[expect(dead_code)]
    role: Link,
    name: String,
    data: Arc<McpServerData<Link>>,
}

impl<'scope, Link: JrLink> McpServerConnect<Link> for McpServerBuilt<Link> {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn connect(&self, mcp_cx: McpContext<Link>) -> DynComponent<crate::mcp::McpServerToClient> {
        DynComponent::new(McpServerConnection {
            data: self.data.clone(),
            mcp_cx,
        })
    }
}

/// An MCP server instance connected to the ACP framework.
pub(crate) struct McpServerConnection<Link: JrLink> {
    data: Arc<McpServerData<Link>>,
    mcp_cx: McpContext<Link>,
}

impl<Link: JrLink> Component<crate::mcp::McpServerToClient> for McpServerConnection<Link> {
    async fn serve(
        self,
        client: impl Component<crate::mcp::McpClientToServer>,
    ) -> Result<(), crate::Error> {
        // Create tokio byte streams that rmcp expects
        let (mcp_server_stream, mcp_client_stream) = tokio::io::duplex(8192);
        let (mcp_server_read, mcp_server_write) = tokio::io::split(mcp_server_stream);
        let (mcp_client_read, mcp_client_write) = tokio::io::split(mcp_client_stream);

        // Create ByteStreams component for the client side
        let byte_streams =
            ByteStreams::new(mcp_client_write.compat_write(), mcp_client_read.compat());

        // Spawn task to connect byte_streams to the provided client
        tokio::spawn(async move {
            let _ = Component::<crate::mcp::McpServerToClient>::serve(byte_streams, client).await;
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

impl<Link: JrLink> ServerHandler for McpServerConnection<Link> {
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParam,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        // Lookup the tool definition, erroring if not found or disabled
        let Some(registered) = self.data.tools.get(&request.name[..]) else {
            return Err(rmcp::model::ErrorData::invalid_params(
                format!("tool `{}` not found", request.name),
                None,
            ));
        };

        // Treat disabled tools as not found
        if !self.data.enabled_tools.is_enabled(&request.name) {
            return Err(rmcp::model::ErrorData::invalid_params(
                format!("tool `{}` not found", request.name),
                None,
            ));
        }

        // Convert input into JSON
        let serde_value = serde_json::to_value(request.arguments).expect("valid json");

        // Execute the user's tool, unless cancellation occurs
        let has_structured_output = registered.has_structured_output;
        match futures::future::select(
            registered.tool.call_tool(serde_value, self.mcp_cx.clone()),
            pin!(context.ct.cancelled()),
        )
        .await
        {
            // If completed successfully
            Either::Left((m, _)) => match m {
                Ok(result) => {
                    // Use structured output only if the tool declared an output_schema
                    if has_structured_output {
                        Ok(CallToolResult::structured(result))
                    } else {
                        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                            result.to_string(),
                        )]))
                    }
                }
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
        // Return only enabled tools
        let tools: Vec<_> = self
            .data
            .tool_models
            .iter()
            .filter(|t| self.data.enabled_tools.is_enabled(&t.name))
            .cloned()
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
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
trait ErasedMcpTool<Link: JrLink>: Send + Sync {
    fn call_tool(
        &self,
        input: serde_json::Value,
        context: McpContext<Link>,
    ) -> BoxFuture<'_, Result<serde_json::Value, crate::Error>>;
}

/// Create an `rmcp` tool model from our [`McpTool`] trait.
fn make_tool_model<Link: JrLink, M: McpTool<Link>>(tool: &M) -> Tool {
    rmcp::model::Tool {
        name: tool.name().into(),
        title: tool.title(),
        description: Some(tool.description().into()),
        input_schema: schema_for_type::<M::Input>(),
        // schema_for_output returns Err for non-object types (strings, integers, etc.)
        // since MCP structured output requires JSON objects. We use .ok() to set
        // output_schema to None for these tools, signaling unstructured output.
        output_schema: schema_for_output::<M::Output>().ok(),
        annotations: None,
        icons: None,
        meta: None,
    }
}

/// Create a [`ErasedMcpTool`] from a [`McpTool`], erasing the type details.
fn make_erased_mcp_tool<'s, Link: JrLink, M: McpTool<Link> + 's>(
    tool: M,
) -> Arc<dyn ErasedMcpTool<Link> + 's> {
    struct ErasedMcpToolImpl<M> {
        tool: M,
    }

    impl<Link, M> ErasedMcpTool<Link> for ErasedMcpToolImpl<M>
    where
        Link: JrLink,
        M: McpTool<Link>,
    {
        fn call_tool(
            &self,
            input: serde_json::Value,
            context: McpContext<Link>,
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

/// MCP tool used for `tool_fn` and `tooL_fn_mut`.
/// Each time it is invoked, it sends a `ToolCall`  message to `call_tx`.
struct ToolFnTool<P, R, Link: JrLink> {
    name: String,
    description: String,
    call_tx: mpsc::Sender<ToolCall<P, R, Link>>,
}

impl<P, R, Link> McpTool<Link> for ToolFnTool<P, R, Link>
where
    Link: JrLink,
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

    async fn call_tool(&self, params: P, mcp_cx: McpContext<Link>) -> Result<R, crate::Error> {
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
