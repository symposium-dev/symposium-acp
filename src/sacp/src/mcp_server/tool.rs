//! MCP tool trait for defining tools.

use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use crate::JrRole;

use super::McpContext;

/// Defines an MCP tool.
pub trait McpTool<Role: JrRole>: Send + Sync {
    /// The type of input the tool accepts.
    type Input: JsonSchema + DeserializeOwned + Send + 'static;

    /// The type of output the tool produces.
    type Output: JsonSchema + Serialize + Send + 'static;

    /// The name of the tool
    fn name(&self) -> String;

    /// A description of what the tool does
    fn description(&self) -> String;

    /// A human-readable title for the tool
    fn title(&self) -> Option<String> {
        None
    }

    /// Define the tool's behavior. You can implement this with an `async fn`.
    fn call_tool(
        &self,
        input: Self::Input,
        context: McpContext<Role>,
    ) -> impl Future<Output = Result<Self::Output, crate::Error>> + Send;
}
