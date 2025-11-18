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

use sacp_tokio::AcpAgent;
use std::str::FromStr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <prompt> <agent-config>", args[0]);
        eprintln!();
        eprintln!("  <prompt>       - The prompt to send to the agent");
        eprintln!("  <agent-config> - Either a command string or JSON (starting with '{{')");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} \"What is 2+2?\" \"python my_agent.py\"", args[0]);
        eprintln!(
            "  {} \"Hello!\" '{{\"type\":\"stdio\",\"name\":\"agent\",\"command\":\"python\",\"args\":[\"agent.py\"],\"env\":[]}}'",
            args[0]
        );
        std::process::exit(1);
    }

    let prompt = &args[1];
    let agent_config = &args[2];

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
