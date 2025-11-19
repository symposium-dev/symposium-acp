//! sacp-tee - A debugging proxy that logs all ACP traffic to a file
//!
//! Usage:
//!   sacp-tee --log-file debug.log -- python agent.py
//!   sacp-tee --json --log-file debug.log -- python agent.py

use anyhow::Result;
use clap::Parser;
use sacp_tokio::AcpAgent;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(name = "sacp-tee")]
#[command(about = "A debugging proxy that logs all ACP traffic", long_about = None)]
struct Args {
    /// Path to the log file where messages will be recorded
    #[arg(short, long, default_value = "sacp-tee.log")]
    log_file: PathBuf,

    /// Use JSON structured logging instead of raw line-by-line logging
    #[arg(long)]
    json: bool,

    /// Command to run for the downstream agent
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.json {
        // Use structured JSON logging mode
        if !args.command.is_empty() {
            eprintln!("Warning: --json mode does not support spawning downstream commands yet");
            eprintln!("The downstream agent should connect via stdio");
        }
        sacp_tee::run(args.log_file).await
    } else {
        // Use raw line-by-line logging mode (default)
        if args.command.is_empty() {
            eprintln!("Error: raw mode requires a downstream command");
            eprintln!("Usage: sacp-tee --log-file debug.log -- python agent.py");
            std::process::exit(1);
        }

        let command_str = args.command.join(" ");
        let downstream = AcpAgent::from_str(&command_str)?;
        sacp_tee::run_raw(args.log_file, downstream).await
    }
}
