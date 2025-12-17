use crate::{JrConnectionCx, JrRole};

/// Context about the ACP and MCP connection available to an MCP server.
#[derive(Clone)]
pub struct McpContext<Role: JrRole> {
    pub(super) acp_url: String,
    pub(super) connection_cx: JrConnectionCx<Role>,
}

impl<Role: JrRole> McpContext<Role> {
    /// The `acp:UUID` that was given.
    pub fn acp_url(&self) -> String {
        self.acp_url.clone()
    }

    /// The ACP connection context, which can be used to send ACP requests and notifications
    /// to your successor.
    pub fn connection_cx(&self) -> JrConnectionCx<Role> {
        self.connection_cx.clone()
    }
}
