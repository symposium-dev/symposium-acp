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
    /// Overrides RUST_LOG and SYMPOSIUM_LOG environment variables
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
        // If --debug is enabled, we need to set up tracing differently
        // We'll defer tracing setup until we can create the debug logger
        if self.debug {
            self.run_with_debug().await
        } else {
            self.run_without_debug().await
        }
    }

    /// Run conductor without debug logging (tracing goes to stderr or file)
    async fn run_without_debug(self) -> anyhow::Result<()> {
        let pid = std::process::id();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());

        // Determine log level: --log flag takes precedence over environment variables
        let log_level = self
            .log
            .clone()
            .or_else(|| std::env::var("SYMPOSIUM_LOG").ok())
            .or_else(|| std::env::var("RUST_LOG").ok());

        // Check for SYMPOSIUM_LOG environment variable (for file logging)
        if let Ok(symposium_log) = std::env::var("SYMPOSIUM_LOG") {
            // Set up file logging to ~/.symposium/logs.$DATE
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."));

            let log_dir = home.join(".symposium");
            std::fs::create_dir_all(&log_dir)?;

            let file_appender = tracing_appender::rolling::daily(log_dir, "logs");

            let effective_log_level = log_level.as_deref().unwrap_or(&symposium_log);

            tracing_subscriber::registry()
                .with(EnvFilter::new(effective_log_level))
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(true)
                        .with_span_events(
                            tracing_subscriber::fmt::format::FmtSpan::NEW
                                | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
                        )
                        .with_writer(file_appender),
                )
                .init();

            tracing::info!(
                pid = %pid,
                cwd = %cwd,
                level = %effective_log_level,
                "Conductor starting with file logging"
            );
        } else {
            // Initialize tracing with env filter support (RUST_LOG=debug, etc.)
            // Important: Always write to stderr to avoid interfering with stdio protocols
            let env_filter = if let Some(ref level) = log_level {
                EnvFilter::new(level)
            } else {
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("conductor=info"))
            };

            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(true)
                        .with_writer(std::io::stderr),
                )
                .init();

            if let Some(ref level) = log_level {
                tracing::info!(pid = %pid, cwd = %cwd, level = %level, "Conductor starting");
            } else {
                tracing::info!(pid = %pid, cwd = %cwd, "Conductor starting");
            }
        }

        self.run()
            .instrument(tracing::info_span!("conductor", pid = %pid, cwd = %cwd))
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    /// Run conductor with debug logging (tracing goes to debug file)
    async fn run_with_debug(self) -> anyhow::Result<()> {
        let pid = std::process::id();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());

        // We need to extract the proxy list first to create the debug logger
        let proxies = match &self.command {
            ConductorCommand::Agent { proxies, .. } => proxies.clone(),
            ConductorCommand::Mcp { .. } => Vec::new(),
        };

        // Create debug logger
        let debug_logger = debug_logger::DebugLogger::new(self.debug_dir.clone(), &proxies)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create debug logger: {}", e))?;

        // Determine log level
        let log_level = self
            .log
            .clone()
            .or_else(|| std::env::var("SYMPOSIUM_LOG").ok())
            .or_else(|| std::env::var("RUST_LOG").ok());

        let env_filter = if let Some(ref level) = log_level {
            EnvFilter::new(level)
        } else {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("conductor=info"))
        };

        // Set up tracing to write to the debug file with "C !" prefix
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_writer(move || debug_logger.create_tracing_writer()),
            )
            .init();

        if let Some(ref level) = log_level {
            tracing::info!(pid = %pid, cwd = %cwd, level = %level, "Conductor starting with debug logging");
        } else {
            tracing::info!(pid = %pid, cwd = %cwd, "Conductor starting with debug logging");
        }

        self.run()
            .instrument(tracing::info_span!("conductor", pid = %pid, cwd = %cwd))
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    async fn run(self) -> Result<(), sacp::Error> {
        match self.command {
            ConductorCommand::Agent { name, proxies } => {
                // Create debug logger if --debug is enabled
                let debug_logger = if self.debug {
                    Some(
                        debug_logger::DebugLogger::new(self.debug_dir.clone(), &proxies)
                            .await
                            .map_err(|e| {
                                sacp::util::internal_error(format!(
                                    "Failed to create debug logger: {}",
                                    e
                                ))
                            })?,
                    )
                } else {
                    None
                };

                // Parse agents and optionally wrap with debug callbacks
                let providers: Vec<AcpAgent> = proxies
                    .into_iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let mut agent = AcpAgent::from_str(&s)?;
                        if let Some(ref logger) = debug_logger {
                            agent = agent.with_debug(logger.create_callback(i.to_string()));
                        }
                        Ok(agent)
                    })
                    .collect::<Result<Vec<_>, sacp::Error>>()?;

                // Create Stdio component with optional debug logging
                let stdio = if let Some(ref logger) = debug_logger {
                    Stdio::new().with_debug(logger.create_callback("C".to_string()))
                } else {
                    Stdio::new()
                };

                Conductor::new(name, providers, None)
                    .into_handler_chain()
                    .connect_to(stdio)?
                    .serve()
                    .await
            }
            ConductorCommand::Mcp { port } => mcp_bridge::run_mcp_bridge(port).await,
        }
    }
}
