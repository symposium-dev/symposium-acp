//! MCP-specific responder types.

use futures::{
    StreamExt,
    channel::{mpsc, oneshot},
    future::BoxFuture,
};

use crate::{ConnectionTo, JrLink, jsonrpc::run::Run, mcp_server::McpContext};

/// A tool call request sent through the channel.
pub(super) struct ToolCall<P, R, Link> {
    pub(crate) params: P,
    pub(crate) mcp_cx: McpContext<Link>,
    pub(crate) result_tx: futures::channel::oneshot::Sender<Result<R, crate::Error>>,
}

/// Responder for a `tool_fn` closure that receives tool calls through a channel
/// and invokes the user's async function.
pub(super) struct ToolFnMutResponder<F, P, R, Link> {
    pub(crate) func: F,
    pub(crate) call_rx: mpsc::Receiver<ToolCall<P, R, Link>>,
    pub(crate) tool_future_fn: Box<
        dyn for<'a> Fn(&'a mut F, P, McpContext<Link>) -> BoxFuture<'a, Result<R, crate::Error>>
            + Send,
    >,
}

impl<F, P, R, Link> Run<Link> for ToolFnMutResponder<F, P, R, Link>
where
    Link: JrLink,
    P: Send,
    R: Send,
    F: Send,
{
    async fn run(self, _cx: ConnectionTo<Link>) -> Result<(), crate::Error> {
        let ToolFnMutResponder {
            mut func,
            mut call_rx,
            tool_future_fn,
        } = self;
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

/// Responder for a `tool_fn` closure that receives tool calls through a channel
/// and invokes the user's async function concurrently.
pub(super) struct ToolFnResponder<F, P, R, Link> {
    pub(crate) func: F,
    pub(crate) call_rx: mpsc::Receiver<ToolCall<P, R, Link>>,
    pub(crate) tool_future_fn: Box<
        dyn for<'a> Fn(&'a F, P, McpContext<Link>) -> BoxFuture<'a, Result<R, crate::Error>>
            + Send
            + Sync,
    >,
}

impl<F, P, R, Link> Run<Link> for ToolFnResponder<F, P, R, Link>
where
    Link: JrLink,
    P: Send,
    R: Send,
    F: Send + Sync,
{
    async fn run(self, _cx: ConnectionTo<Link>) -> Result<(), crate::Error> {
        let ToolFnResponder {
            func,
            call_rx,
            tool_future_fn,
        } = self;
        crate::util::process_stream_concurrently(
            call_rx,
            async |tool_call| {
                fn hack<'a, F, P, R, Link>(
                    func: &'a F,
                    params: P,
                    mcp_cx: McpContext<Link>,
                    tool_future_fn: &'a (
                            dyn Fn(
                        &'a F,
                        P,
                        McpContext<Link>,
                    ) -> BoxFuture<'a, Result<R, crate::Error>>
                                + Send
                                + Sync
                        ),
                    result_tx: oneshot::Sender<Result<R, crate::Error>>,
                ) -> BoxFuture<'a, ()>
                where
                    Link: JrLink,
                    P: Send,
                    R: Send,
                    F: Send + Sync,
                {
                    Box::pin(async move {
                        let result = tool_future_fn(func, params, mcp_cx).await;
                        // Ignore send errors - the receiver may have been dropped
                        let _ = result_tx.send(result);
                    })
                }

                let ToolCall {
                    params,
                    mcp_cx,
                    result_tx,
                } = tool_call;

                hack(&func, params, mcp_cx, &*tool_future_fn, result_tx).await;
                Ok(())
            },
            |a, b| Box::pin(a(b)),
        )
        .await
    }
}
