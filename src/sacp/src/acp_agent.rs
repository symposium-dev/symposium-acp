//! Utilities for connecting to ACP agents and proxies.
//!
//! This module provides [`AcpAgent`], a convenient wrapper around [`McpServer`]
//! that can be parsed from either a command string or JSON configuration.

use std::path::PathBuf;
use std::str::FromStr;

use futures::AsyncRead;
use futures::AsyncWrite;
use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Configuration for connecting to an ACP agent or proxy.
///
/// This is a wrapper around [`McpServer`] that provides convenient parsing
/// from command-line strings or JSON configurations.
///
/// # Examples
///
/// Parse from a command string:
/// ```
/// # use sacp::acp_agent::AcpAgent;
/// # use std::str::FromStr;
/// let agent = AcpAgent::from_str("python my_agent.py --verbose").unwrap();
/// ```
///
/// Parse from JSON:
/// ```
/// # use sacp::acp_agent::AcpAgent;
/// # use std::str::FromStr;
/// let agent = AcpAgent::from_str(r#"{"type": "stdio", "name": "my-agent", "command": "python", "args": ["my_agent.py"], "env": []}"#).unwrap();
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct AcpAgent {
    #[serde(flatten)]
    server: crate::McpServer,
}

impl AcpAgent {
    /// Create a new `AcpAgent` from an [`McpServer`] configuration.
    pub fn new(server: crate::McpServer) -> Self {
        Self { server }
    }

    /// Get the underlying [`McpServer`] configuration.
    pub fn server(&self) -> &crate::McpServer {
        &self.server
    }

    /// Convert into the underlying [`McpServer`] configuration.
    pub fn into_server(self) -> crate::McpServer {
        self.server
    }

    /// Spawn the agent/proxy and create a [`JrConnection`] to communicate with it.
    ///
    /// Returns the connection and a handle to clean up the spawned process.
    ///
    /// # Limitations
    ///
    /// Currently only supports `McpServer::Stdio` variant. Other variants (HTTP, SSE)
    /// will return an error.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use sacp::acp_agent::AcpAgent;
    /// # use std::str::FromStr;
    /// # async fn example() -> Result<(), sacp::Error> {
    /// let agent = AcpAgent::from_str("python agent.py")?;
    /// let (connection, _cleanup) = agent.spawn()?;
    /// // Use connection...
    /// # Ok(())
    /// # }
    /// ```
    pub fn spawn(
        &self,
    ) -> Result<
        (
            crate::JrConnection<
                impl AsyncWrite + Send + 'static,
                impl AsyncRead + Send + 'static,
                crate::NullHandler,
            >,
            SpawnedProcess,
        ),
        crate::Error,
    > {
        match &self.server {
            crate::McpServer::Stdio {
                command,
                args,
                env,
                name: _,
            } => {
                let mut cmd = tokio::process::Command::new(command);
                cmd.args(args);
                for env_var in env {
                    cmd.env(&env_var.name, &env_var.value);
                }
                cmd.stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped());

                let mut child = cmd.spawn().map_err(crate::Error::into_internal_error)?;

                let child_stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| crate::util::internal_error("Failed to open stdin"))?;
                let child_stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| crate::util::internal_error("Failed to open stdout"))?;

                let connection =
                    crate::JrConnection::new(child_stdin.compat_write(), child_stdout.compat());

                Ok((connection, SpawnedProcess { child }))
            }
            crate::McpServer::Http { .. } => Err(crate::util::internal_error(
                "HTTP transport not yet supported by AcpAgent::spawn",
            )),
            crate::McpServer::Sse { .. } => Err(crate::util::internal_error(
                "SSE transport not yet supported by AcpAgent::spawn",
            )),
        }
    }
}

/// Handle to a spawned process that will be killed when dropped.
pub struct SpawnedProcess {
    child: Child,
}

impl Drop for SpawnedProcess {
    fn drop(&mut self) {
        let _: Result<_, _> = self.child.start_kill();
    }
}

impl FromStr for AcpAgent {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();

        // If it starts with '{', try to parse as JSON
        if trimmed.starts_with('{') {
            let server: crate::McpServer = serde_json::from_str(trimmed)
                .map_err(|e| crate::util::internal_error(format!("Failed to parse JSON: {}", e)))?;
            return Ok(Self { server });
        }

        // Otherwise, parse as a command string
        parse_command_string(trimmed)
    }
}

fn parse_command_string(s: &str) -> Result<AcpAgent, crate::Error> {
    // Split the command string into words, respecting quotes
    let parts = shell_words::split(s)
        .map_err(|e| crate::util::internal_error(format!("Failed to parse command: {}", e)))?;

    if parts.is_empty() {
        return Err(crate::util::internal_error(
            "Command string cannot be empty",
        ));
    }

    let command = PathBuf::from(&parts[0]);
    let args = parts[1..].to_vec();

    // Generate a name from the command
    let name = command
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("agent")
        .to_string();

    Ok(AcpAgent {
        server: crate::McpServer::Stdio {
            name,
            command,
            args,
            env: vec![],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_command() {
        let agent = AcpAgent::from_str("python agent.py").unwrap();
        match agent.server {
            crate::McpServer::Stdio {
                name,
                command,
                args,
                env,
            } => {
                assert_eq!(name, "python");
                assert_eq!(command, PathBuf::from("python"));
                assert_eq!(args, vec!["agent.py"]);
                assert_eq!(env, vec![]);
            }
            _ => panic!("Expected Stdio variant"),
        }
    }

    #[test]
    fn test_parse_command_with_args() {
        let agent = AcpAgent::from_str("node server.js --port 8080 --verbose").unwrap();
        match agent.server {
            crate::McpServer::Stdio {
                name,
                command,
                args,
                env,
            } => {
                assert_eq!(name, "node");
                assert_eq!(command, PathBuf::from("node"));
                assert_eq!(args, vec!["server.js", "--port", "8080", "--verbose"]);
                assert_eq!(env, vec![]);
            }
            _ => panic!("Expected Stdio variant"),
        }
    }

    #[test]
    fn test_parse_command_with_quotes() {
        let agent = AcpAgent::from_str(r#"python "my agent.py" --name "Test Agent""#).unwrap();
        match agent.server {
            crate::McpServer::Stdio {
                name,
                command,
                args,
                env,
            } => {
                assert_eq!(name, "python");
                assert_eq!(command, PathBuf::from("python"));
                assert_eq!(args, vec!["my agent.py", "--name", "Test Agent"]);
                assert_eq!(env, vec![]);
            }
            _ => panic!("Expected Stdio variant"),
        }
    }

    #[test]
    fn test_parse_json_stdio() {
        let json = r#"{
            "type": "stdio",
            "name": "my-agent",
            "command": "/usr/bin/python",
            "args": ["agent.py", "--verbose"],
            "env": []
        }"#;
        let agent = AcpAgent::from_str(json).unwrap();
        match agent.server {
            crate::McpServer::Stdio {
                name,
                command,
                args,
                env,
            } => {
                assert_eq!(name, "my-agent");
                assert_eq!(command, PathBuf::from("/usr/bin/python"));
                assert_eq!(args, vec!["agent.py", "--verbose"]);
                assert_eq!(env, vec![]);
            }
            _ => panic!("Expected Stdio variant"),
        }
    }

    #[test]
    fn test_parse_json_http() {
        let json = r#"{
            "type": "http",
            "name": "remote-agent",
            "url": "https://example.com/agent",
            "headers": []
        }"#;
        let agent = AcpAgent::from_str(json).unwrap();
        match agent.server {
            crate::McpServer::Http { name, url, headers } => {
                assert_eq!(name, "remote-agent");
                assert_eq!(url, "https://example.com/agent");
                assert_eq!(headers, vec![]);
            }
            _ => panic!("Expected Http variant"),
        }
    }
}
