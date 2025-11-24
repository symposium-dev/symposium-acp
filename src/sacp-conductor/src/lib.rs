//! # sacp-conductor
//!
//! Binary for orchestrating ACP proxy chains.
//!
//! ## What is the conductor?
//!
//! The conductor is a tool that manages proxy chains - it spawns proxy components and the base agent,
//! then routes messages between them. From the editor's perspective, the conductor appears as a single ACP agent.
//!
//! ```text
//! Editor ← stdio → Conductor → Proxy 1 → Proxy 2 → Agent
//! ```
//!
//! ## Usage
//!
//! ### Agent Mode
//!
//! Orchestrate a chain of proxies in front of an agent:
//!
//! ```bash
//! # Chain format: proxy1 proxy2 ... agent
//! sacp-conductor agent "python proxy1.py" "python proxy2.py" "python base-agent.py"
//! ```
//!
//! The conductor:
//! 1. Spawns each component as a subprocess
//! 2. Connects them in a chain
//! 3. Presents as a single agent on stdin/stdout
//! 4. Manages the lifecycle of all processes
//!
//! ### MCP Bridge Mode
//!
//! Connect stdio to a TCP-based MCP server:
//!
//! ```bash
//! # Bridge stdio to MCP server on localhost:8080
//! sacp-conductor mcp 8080
//! ```
//!
//! This allows stdio-based tools to communicate with TCP MCP servers.
//!
//! ## How It Works
//!
//! **Component Communication:**
//! - Editor talks to conductor via stdio
//! - Conductor uses `_proxy/successor/*` protocol extensions to route messages
//! - Each proxy can intercept, transform, or forward messages
//! - Final agent receives standard ACP messages
//!
//! **Process Management:**
//! - All components are spawned as child processes
//! - When conductor exits, all children are terminated
//! - Errors in any component bring down the entire chain
//!
//! ## Example Use Case
//!
//! Add Sparkle embodiment + custom tools to any agent:
//!
//! ```bash
//! sacp-conductor agent \
//!   "sparkle-acp-proxy" \
//!   "my-custom-tools-proxy" \
//!   "claude-agent"
//! ```
//!
//! This creates a stack where:
//! 1. Sparkle proxy injects MCP servers and prepends embodiment
//! 2. Custom tools proxy adds domain-specific functionality
//! 3. Base agent handles the actual AI responses
//!
//! ## Related Crates
//!
//! - **[sacp-proxy](https://crates.io/crates/sacp-proxy)** - Framework for building proxy components
//! - **[sacp](https://crates.io/crates/sacp)** - Core ACP SDK
//! - **[sacp-tokio](https://crates.io/crates/sacp-tokio)** - Tokio utilities for process spawning

use std::path::PathBuf;
use std::str::FromStr;

use crate::conductor::Conductor;

/// Core conductor logic for orchestrating proxy chains
pub mod conductor;
/// Debug logging for conductor
mod debug_logger;
/// MCP bridge functionality for TCP-based MCP servers
mod mcp_bridge;

use clap::{Parser, Subcommand};
use sacp_tokio::{AcpAgent, Stdio};
use tracing::Instrument;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Mode for the MCP bridge.
#[derive(Debug, Clone)]
pub enum McpBridgeMode {
    /// Use stdio-based MCP bridge with a conductor subprocess.
    Stdio {
        /// Command and args to spawn conductor MCP bridge processes.
        /// E.g., vec!["conductor"] or vec!["cargo", "run", "-p", "conductor", "--"]
        conductor_command: Vec<String>,
    },

    /// Use HTTP-based MCP bridge
    Http,
}

impl Default for McpBridgeMode {
    fn default() -> Self {
        let argv0 = std::env::current_exe()
            .expect("valid current executable path")
            .display()
            .to_string();
        McpBridgeMode::Stdio {
            conductor_command: vec![argv0],
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct ConductorArgs {
    /// Enable debug logging of all stdin/stdout/stderr from components
    #[arg(long)]
    pub debug: bool,

    /// Directory for debug log files (defaults to current directory)
    #[arg(long)]
    pub debug_dir: Option<PathBuf>,

    /// Set log level (e.g., "trace", "debug", "info", "warn", "error", or module-specific like "conductor=debug")
    /// Only applies when --debug is enabled
    #[arg(long)]
    pub log: Option<String>,

    #[command(subcommand)]
    pub command: ConductorCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConductorCommand {
    /// Run as agent orchestrator managing a proxy chain
    Agent {
        /// Name of the agent
        #[arg(short, long, default_value = "conductor")]
        name: String,

        /// List of proxy commands to chain together
        proxies: Vec<String>,
    },
    /// Run as MCP bridge connecting stdio to TCP
    Mcp {
        /// TCP port to connect to on localhost
        port: u16,
    },
}

impl ConductorArgs {
    /// Main entry point that sets up tracing and runs the conductor
    pub async fn main(self) -> anyhow::Result<()> {
        let pid = std::process::id();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());

        // Only set up tracing if --debug is enabled
        let debug_logger = if self.debug {
            // Extract proxy list to create the debug logger
            let proxies = match &self.command {
                ConductorCommand::Agent { proxies, .. } => proxies.clone(),
                ConductorCommand::Mcp { .. } => Vec::new(),
            };

            // Create debug logger
            Some(
                debug_logger::DebugLogger::new(self.debug_dir.clone(), &proxies)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to create debug logger: {}", e))?,
            )
        } else {
            None
        };

        if let Some(debug_logger) = &debug_logger {
            // Set up log level from --log flag, defaulting to "info"
            let log_level = self.log.as_deref().unwrap_or("info");

            // Set up tracing to write to the debug file with "C !" prefix
            let tracing_writer = debug_logger.create_tracing_writer();
            tracing_subscriber::registry()
                .with(EnvFilter::new(log_level))
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(true)
                        .with_writer(move || tracing_writer.clone()),
                )
                .init();

            tracing::info!(pid = %pid, cwd = %cwd, level = %log_level, "Conductor starting with debug logging");
        };

        self.run(debug_logger.as_ref())
            .instrument(tracing::info_span!("conductor", pid = %pid, cwd = %cwd))
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    async fn run(
        self,
        debug_logger: Option<&debug_logger::DebugLogger>,
    ) -> Result<(), sacp::Error> {
        match self.command {
            ConductorCommand::Agent { name, proxies } => {
                // Parse agents and optionally wrap with debug callbacks
                let providers: Vec<AcpAgent> = proxies
                    .into_iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let mut agent = AcpAgent::from_str(&s)?;
                        if let Some(logger) = debug_logger {
                            agent = agent.with_debug(logger.create_callback(i.to_string()));
                        }
                        Ok(agent)
                    })
                    .collect::<Result<Vec<_>, sacp::Error>>()?;

                // Create Stdio component with optional debug logging
                let stdio = if let Some(logger) = debug_logger {
                    Stdio::new().with_debug(logger.create_callback("C".to_string()))
                } else {
                    Stdio::new()
                };

                Conductor::new(name, providers, Default::default())
                    .into_handler_chain()
                    .connect_to(stdio)?
                    .serve()
                    .await
            }
            ConductorCommand::Mcp { port } => mcp_bridge::run_mcp_bridge(port).await,
        }
    }
}
