//! MCP-specific responder types.

use std::future::Future;

use futures::{StreamExt, channel::mpsc};

use crate::{JrConnectionCx, JrRole, jsonrpc::responder::JrResponder, mcp_server::McpContext};

/// A tool call request sent through the channel.
pub struct ToolCall<P, R, Role: JrRole> {
    pub(crate) params: P,
    pub(crate) mcp_cx: McpContext<Role>,
    pub(crate) result_tx: futures::channel::oneshot::Sender<Result<R, crate::Error>>,
}

/// Responder for a `tool_fn` closure that receives tool calls through a channel
/// and invokes the user's async function.
pub struct ToolFnResponder<F, P, R, Role: JrRole> {
    pub(crate) func: F,
    pub(crate) call_rx: mpsc::Receiver<ToolCall<P, R, Role>>,
}

impl<F, P, R, Role, Fut> JrResponder<Role> for ToolFnResponder<F, P, R, Role>
where
    Role: JrRole,
    P: Send,
    R: Send,
    F: FnMut(P, McpContext<Role>) -> Fut + Send,
    Fut: Future<Output = Result<R, crate::Error>> + Send,
{
    async fn run(mut self, _cx: JrConnectionCx<Role>) -> Result<(), crate::Error> {
        while let Some(ToolCall {
            params,
            mcp_cx,
            result_tx,
        }) = self.call_rx.next().await
        {
            let result = (self.func)(params, mcp_cx).await;
            result_tx
                .send(result)
                .map_err(|_| crate::util::internal_error("failed to send MCP result"))?;
        }
        Ok(())
    }
}
