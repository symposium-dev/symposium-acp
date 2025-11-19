//! YOPO (You Only Prompt Once) - A simple ACP client for one-shot prompts
//!
//! This client:
//! - Takes a prompt and agent configuration as arguments
//! - Spawns the agent
//! - Sends the prompt
//! - Auto-approves all permission requests
//! - Prints content progressively as it arrives
//! - Runs until the agent completes
//!
//! # Usage
//!
//! With a command:
//! ```bash
//! yopo "What is 2+2?" "python my_agent.py"
//! ```
//!
//! With JSON config:
//! ```bash
//! yopo "Hello!" '{"type":"stdio","name":"my-agent","command":"python","args":["agent.py"],"env":[]}'
//! ```

use clap::Parser;
use sacp_tokio::AcpAgent;
use std::str::FromStr;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about = "YOPO - You Only Prompt Once", long_about = None)]
struct Args {
    /// The prompt to send to the agent
    prompt: String,

    /// Agent configuration (command string or JSON)
    agent_config: String,

    /// Set logging level (trace, debug, info, warn, error)
    #[arg(short, long)]
    log: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Initialize tracing to stderr
    let env_filter = if let Some(level) = args.log {
        EnvFilter::new(format!("yopo={}", level))
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("yopo=info"))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(std::io::stderr),
        )
        .init();

    let prompt = &args.prompt;
    let agent_config = &args.agent_config;

    // Parse the agent configuration
    let agent = AcpAgent::from_str(agent_config)?;

    eprintln!("🚀 Spawning agent and running prompt...");

    // Use the library function with callback to print progressively
    yopo::prompt_with_callback(agent, prompt.as_str(), |block| async move {
        print!("{}", yopo::content_block_to_string(&block));
    })
    .await?;

    println!(); // Final newline
    eprintln!("✅ Agent completed!");

    Ok(())
}
