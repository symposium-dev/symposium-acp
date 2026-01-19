//! # elizacp
//!
//! A classic Eliza chatbot implemented as an ACP (Agent-Client Protocol) agent.
//!
//! ## Overview
//!
//! Elizacp provides a simple, predictable agent implementation that's useful for:
//!
//! - **Testing ACP clients** - Lightweight agent with deterministic pattern-based responses
//! - **Protocol development** - Verify ACP implementations without heavy AI infrastructure
//! - **Learning ACP** - Clean example of implementing the Agent-Client Protocol
//!
//! ## Features
//!
//! - **Classic Eliza patterns** - Pattern matching and reflection-based responses
//! - **Full ACP support** - Session management, initialization, and prompt handling
//! - **Per-session state** - Each session maintains its own Eliza instance
//! - **Extensible patterns** - Easy to add new response patterns
//!
//! ## Usage
//!
//! ```bash
//! # Build and run
//! cargo run -p elizacp
//!
//! # With debug logging
//! cargo run -p elizacp -- --debug
//! ```
//!
//! The agent communicates over stdin/stdout using JSON-RPC, following the ACP specification.
//!
//! ## Implementation
//!
//! The agent maintains a `HashMap<SessionId, Eliza>` to track per-session state.
//! Each session gets its own Eliza instance with independent conversation state.

use anyhow::Result;
use clap::Parser;
use elizacp::ElizaAgent;
use sacp::Serve;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about = "Eliza chatbot as an ACP agent", long_about = None)]
struct Args {
    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,

    /// Use deterministic responses (fixed seed for testing)
    #[arg(long)]
    deterministic: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Run as ACP agent over stdio
    Acp,
    /// Run interactive chat TUI
    Chat,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing to stderr
    let env_filter = if args.debug {
        EnvFilter::new("elizacp=debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("elizacp=info"))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(std::io::stderr),
        )
        .init();

    match args.command {
        Command::Chat => {
            return elizacp::chat::run(args.deterministic);
        }
        Command::Acp => {
            tracing::info!("Elizacp starting");
            ElizaAgent::new(args.deterministic)
                .serve(sacp_tokio::Stdio::new())
                .await?;
        }
    }

    Ok(())
}
