use clap::Parser;
use sacp_conductor::ConductorArgs;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ConductorArgs::parse().main().await
}
