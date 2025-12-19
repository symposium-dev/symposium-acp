//! MCP-specific responder types.

use futures::{StreamExt, channel::mpsc, future::BoxFuture};

use crate::{JrConnectionCx, JrRole, jsonrpc::responder::JrResponder, mcp_server::McpContext};

/// A tool call request sent through the channel.
pub struct ToolCall<P, R, Role> {
    pub(crate) params: P,
    pub(crate) mcp_cx: McpContext<Role>,
    pub(crate) result_tx: futures::channel::oneshot::Sender<Result<R, crate::Error>>,
}

/// Responder for a `tool_fn` closure that receives tool calls through a channel
/// and invokes the user's async function.
pub struct ToolFnResponder<F, P, R, Role> {
    pub(crate) func: F,
    pub(crate) call_rx: mpsc::Receiver<ToolCall<P, R, Role>>,
    pub(crate) tool_future_fn: Box<dyn for<'a> Fn(&'a mut F, P, McpContext<Role>) -> BoxFuture<'a, Result<R, crate::Error>> + Send>,
}

impl<F, P, R, Role> JrResponder<Role> for ToolFnResponder<F, P, R, Role>
where
    Role: JrRole,
    P: Send,
    R: Send,
    F: Send,
{
    async fn run(self, _cx: JrConnectionCx<Role>) -> Result<(), crate::Error> {
        let ToolFnResponder { mut func, mut call_rx, tool_future_fn } = self;
        while let Some(ToolCall {
            params,
            mcp_cx,
            result_tx,
        }) = call_rx.next().await
        {
            let result = tool_future_fn(&mut func, params, mcp_cx).await;
            result_tx
                .send(result)
                .map_err(|_| crate::util::internal_error("failed to send MCP result"))?;
        }
        Ok(())
    }
}
