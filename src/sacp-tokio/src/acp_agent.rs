//! Utilities for connecting to ACP agents and proxies.
//!
//! This module provides [`AcpAgent`], a convenient wrapper around [`sacp::schema::McpServer`]
//! that can be parsed from either a command string or JSON configuration.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::process::Child;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Direction of a line being sent or received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineDirection {
    /// Line being sent to the agent (stdin)
    Stdin,
    /// Line being received from the agent (stdout)
    Stdout,
    /// Line being received from the agent (stderr)
    Stderr,
}

/// A component representing an external ACP agent running in a separate process.
///
/// `AcpAgent` implements the [`sacp::Component`] trait for spawning and communicating with
/// external agents or proxies via stdio. It handles process spawning, stream setup, and
/// byte stream serialization automatically. This is the primary way to connect to agents
/// that run as separate executables.
///
/// This is a wrapper around [`sacp::schema::McpServer`] that provides convenient parsing
/// from command-line strings or JSON configurations.
///
/// # Use Cases
///
/// - **External agents**: Connect to agents written in any language (Python, Node.js, Rust, etc.)
/// - **Proxy chains**: Spawn intermediate proxies that transform or intercept messages
/// - **Conductor components**: Use with [`sacp_conductor::Conductor`] to build proxy chains
/// - **Subprocess isolation**: Run potentially untrusted code in a separate process
///
/// # Examples
///
/// Parse from a command string:
/// ```
/// # use sacp_tokio::AcpAgent;
/// # use std::str::FromStr;
/// let agent = AcpAgent::from_str("python my_agent.py --verbose").unwrap();
/// ```
///
/// Parse from JSON:
/// ```
/// # use sacp_tokio::AcpAgent;
/// # use std::str::FromStr;
/// let agent = AcpAgent::from_str(r#"{"type": "stdio", "name": "my-agent", "command": "python", "args": ["my_agent.py"], "env": []}"#).unwrap();
/// ```
///
/// Use as a component to connect to an external agent:
/// ```no_run
/// # use sacp::JrHandlerChain;
/// # use sacp_tokio::AcpAgent;
/// # use std::str::FromStr;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let agent = AcpAgent::from_str("python my_agent.py")?;
///
/// // The agent process will be spawned automatically when served
/// JrHandlerChain::new()
///     .connect_to(agent)?
///     .with_client(|cx| async move {
///         // Use the connection to communicate with the agent process
///         Ok(())
///     })
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// [`sacp_conductor::Conductor`]: https://docs.rs/sacp-conductor/latest/sacp_conductor/struct.Conductor.html
pub struct AcpAgent {
    server: sacp::schema::McpServer,
    debug_callback: Option<Arc<dyn Fn(&str, LineDirection) + Send + Sync + 'static>>,
}

impl AcpAgent {
    /// Create a new `AcpAgent` from an [`sacp::schema::McpServer`] configuration.
    pub fn new(server: sacp::schema::McpServer) -> Self {
        Self {
            server,
            debug_callback: None,
        }
    }

    /// Get the underlying [`sacp::schema::McpServer`] configuration.
    pub fn server(&self) -> &sacp::schema::McpServer {
        &self.server
    }

    /// Convert into the underlying [`sacp::schema::McpServer`] configuration.
    pub fn into_server(self) -> sacp::schema::McpServer {
        self.server
    }

    /// Add a debug callback that will be invoked for each line sent/received.
    ///
    /// The callback receives the line content and the direction (stdin/stdout/stderr).
    /// This is useful for logging, debugging, or monitoring agent communication.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use sacp_tokio::{AcpAgent, LineDirection};
    /// # use std::str::FromStr;
    /// let agent = AcpAgent::from_str("python my_agent.py")
    ///     .unwrap()
    ///     .with_debug(|line, direction| {
    ///         eprintln!("{:?}: {}", direction, line);
    ///     });
    /// ```
    pub fn with_debug<F>(mut self, callback: F) -> Self
    where
        F: Fn(&str, LineDirection) + Send + Sync + 'static,
    {
        self.debug_callback = Some(Arc::new(callback));
        self
    }

    /// Spawn the process and get stdio streams.
    /// Used internally by the Component trait implementation.
    pub fn spawn_process(
        &self,
    ) -> Result<
        (
            tokio::process::ChildStdin,
            tokio::process::ChildStdout,
            tokio::process::ChildStderr,
            Child,
        ),
        sacp::Error,
    > {
        match &self.server {
            sacp::schema::McpServer::Stdio {
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
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());

                let mut child = cmd.spawn().map_err(sacp::Error::into_internal_error)?;

                let child_stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| sacp::util::internal_error("Failed to open stdin"))?;
                let child_stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| sacp::util::internal_error("Failed to open stdout"))?;
                let child_stderr = child
                    .stderr
                    .take()
                    .ok_or_else(|| sacp::util::internal_error("Failed to open stderr"))?;

                Ok((child_stdin, child_stdout, child_stderr, child))
            }
            sacp::schema::McpServer::Http { .. } => Err(sacp::util::internal_error(
                "HTTP transport not yet supported by AcpAgent",
            )),
            sacp::schema::McpServer::Sse { .. } => Err(sacp::util::internal_error(
                "SSE transport not yet supported by AcpAgent",
            )),
        }
    }
}

/// A future that holds a `Child` process and never resolves.
/// When dropped, the child process is killed.
struct ChildHolder {
    _child: Child,
}

impl Future for ChildHolder {
    type Output = Result<(), sacp::Error>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Never ready - just hold the child process alive
        Poll::Pending
    }
}

impl Drop for ChildHolder {
    fn drop(&mut self) {
        let _: Result<_, _> = self._child.start_kill();
    }
}

impl sacp::Component for AcpAgent {
    async fn serve(self, client: impl sacp::Component) -> Result<(), sacp::Error> {
        use futures::AsyncBufReadExt;
        use futures::AsyncWriteExt;
        use futures::StreamExt;
        use futures::io::BufReader;

        let (child_stdin, child_stdout, child_stderr, child) = self.spawn_process()?;

        // Hold the child process - it will be killed when this future completes
        let _child_holder = ChildHolder { _child: child };

        // Spawn a task to read stderr if we have a debug callback
        if let Some(callback) = self.debug_callback.clone() {
            tokio::spawn(async move {
                let stderr_reader = BufReader::new(child_stderr.compat());
                let mut stderr_lines = stderr_reader.lines();
                while let Some(line_result) = stderr_lines.next().await {
                    if let Ok(line) = line_result {
                        callback(&line, LineDirection::Stderr);
                    }
                }
            });
        }

        // Convert stdio to line streams with optional debug inspection
        let incoming_lines = if let Some(callback) = self.debug_callback.clone() {
            Box::pin(
                BufReader::new(child_stdout.compat())
                    .lines()
                    .inspect(move |result| {
                        if let Ok(line) = result {
                            callback(line, LineDirection::Stdout);
                        }
                    }),
            )
                as std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<String>> + Send>>
        } else {
            Box::pin(BufReader::new(child_stdout.compat()).lines())
        };

        // Create a sink that writes lines (with newlines) to stdin with optional debug logging
        let outgoing_sink = if let Some(callback) = self.debug_callback.clone() {
            Box::pin(futures::sink::unfold(
                (child_stdin.compat_write(), callback),
                async move |(mut writer, callback), line: String| {
                    callback(&line, LineDirection::Stdin);
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\n');
                    writer.write_all(&bytes).await?;
                    Ok::<_, std::io::Error>((writer, callback))
                },
            ))
                as std::pin::Pin<Box<dyn futures::Sink<String, Error = std::io::Error> + Send>>
        } else {
            Box::pin(futures::sink::unfold(
                child_stdin.compat_write(),
                async move |mut writer, line: String| {
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\n');
                    writer.write_all(&bytes).await?;
                    Ok::<_, std::io::Error>(writer)
                },
            ))
        };

        // Create the Lines component and serve it
        sacp::Lines::new(outgoing_sink, incoming_lines)
            .serve(client)
            .await
    }
}

impl AcpAgent {
    /// Create an `AcpAgent` from an iterator of command-line arguments.
    ///
    /// Leading arguments of the form `NAME=value` are parsed as environment variables.
    /// The first non-env argument is the command, and the rest are arguments.
    ///
    /// # Example
    ///
    /// ```
    /// # use sacp_tokio::AcpAgent;
    /// let agent = AcpAgent::from_args([
    ///     "RUST_LOG=debug",
    ///     "cargo",
    ///     "run",
    ///     "-p",
    ///     "my-crate",
    /// ]).unwrap();
    /// ```
    pub fn from_args<I, T>(args: I) -> Result<Self, sacp::Error>
    where
        I: IntoIterator<Item = T>,
        T: ToString,
    {
        let args: Vec<String> = args.into_iter().map(|s| s.to_string()).collect();

        if args.is_empty() {
            return Err(sacp::util::internal_error("Arguments cannot be empty"));
        }

        let mut env = vec![];
        let mut command_idx = 0;

        // Parse leading FOO=bar arguments as environment variables
        for (i, arg) in args.iter().enumerate() {
            if let Some((name, value)) = parse_env_var(arg) {
                env.push(sacp::schema::EnvVariable {
                    name,
                    value,
                    meta: None,
                });
                command_idx = i + 1;
            } else {
                break;
            }
        }

        if command_idx >= args.len() {
            return Err(sacp::util::internal_error(
                "No command found (only environment variables provided)",
            ));
        }

        let command = PathBuf::from(&args[command_idx]);
        let cmd_args = args[command_idx + 1..].to_vec();

        // Generate a name from the command
        let name = command
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("agent")
            .to_string();

        Ok(AcpAgent {
            server: sacp::schema::McpServer::Stdio {
                name,
                command,
                args: cmd_args,
                env,
            },
            debug_callback: None,
        })
    }
}

/// Parse a string as an environment variable assignment (NAME=value).
/// Returns None if it doesn't match the pattern.
fn parse_env_var(s: &str) -> Option<(String, String)> {
    // Must contain '=' and the part before must be a valid env var name
    let eq_pos = s.find('=')?;
    if eq_pos == 0 {
        return None;
    }

    let name = &s[..eq_pos];
    let value = &s[eq_pos + 1..];

    // Env var names must start with a letter or underscore, and contain only
    // alphanumeric characters and underscores
    let mut chars = name.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }

    Some((name.to_string(), value.to_string()))
}

impl FromStr for AcpAgent {
    type Err = sacp::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();

        // If it starts with '{', try to parse as JSON
        if trimmed.starts_with('{') {
            let server: sacp::schema::McpServer = serde_json::from_str(trimmed)
                .map_err(|e| sacp::util::internal_error(format!("Failed to parse JSON: {}", e)))?;
            return Ok(Self {
                server,
                debug_callback: None,
            });
        }

        // Otherwise, parse as a command string
        let parts = shell_words::split(trimmed)
            .map_err(|e| sacp::util::internal_error(format!("Failed to parse command: {}", e)))?;

        Self::from_args(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_command() {
        let agent = AcpAgent::from_str("python agent.py").unwrap();
        match agent.server {
            sacp::schema::McpServer::Stdio {
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
            sacp::schema::McpServer::Stdio {
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
            sacp::schema::McpServer::Stdio {
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
            sacp::schema::McpServer::Stdio {
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
            sacp::schema::McpServer::Http { name, url, headers } => {
                assert_eq!(name, "remote-agent");
                assert_eq!(url, "https://example.com/agent");
                assert_eq!(headers, vec![]);
            }
            _ => panic!("Expected Http variant"),
        }
    }
}
