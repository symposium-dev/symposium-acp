use std::path::{Path, PathBuf};

use extension_trait::extension_trait;
use sacp::ClientToAgent;
use sacp::mcp_server::McpServiceRegistry;
use sacp::{JrConnectionCx, schema::NewSessionRequest};

#[extension_trait]
pub impl AcpClientExt for JrConnectionCx<ClientToAgent> {
    fn new_session(&self) -> AcpSessionBuilder {
        AcpSessionBuilder::new(self.clone())
    }
}

pub struct AcpSessionBuilder {
    request: NewSessionRequest,
    cx: JrConnectionCx<ClientToAgent>,
}

impl AcpSessionBuilder {
    pub fn new(cx: JrConnectionCx<ClientToAgent>) -> Self {
        Self {
            request: NewSessionRequest {
                cwd: PathBuf::new(),
                mcp_servers: vec![],
                meta: None,
            },
            cx,
        }
    }

    /// Set the current working directory for the session.
    pub fn cwd(mut self, cwd: impl AsRef<Path>) -> Self {
        self.request.cwd = cwd.as_ref().to_path_buf();
        self
    }

    /// Set the current working directory for the session.
    pub fn mcp_servers(
        mut self,
        registry: impl AsRef<McpServiceRegistry<ClientToAgent>>,
    ) -> Result<Self, sacp::Error> {
        let registry = registry.as_ref().clone();
        registry.add_registered_mcp_servers_to(&mut self.request);
        self.cx.add_dynamic_handler(registry)?;
        Ok(self)
    }

    /// Set the meta data for the session.
    pub fn meta(mut self, meta: serde_json::Value) -> Self {
        self.request.meta = Some(meta);
        self
    }

    /// Read the current value of the [`NewSessionRequest`] that will be sent.
    pub fn get_request(&self) -> &NewSessionRequest {
        &self.request
    }
}

pub struct AcpSession {}
