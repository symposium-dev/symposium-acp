//! SACP Trace Viewer
//!
//! Interactive sequence diagram viewer for SACP trace files.
//!
//! Usage:
//! ```bash
//! sacp-trace-viewer ./trace.jsons
//! ```
//!
//! This starts a local HTTP server and opens a browser to view the trace
//! as an interactive sequence diagram.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Interactive sequence diagram viewer for SACP trace files"
)]
struct Args {
    /// Path to the trace file (.jsons)
    trace_file: PathBuf,

    /// Port to serve on (default: auto-select)
    #[arg(short, long)]
    port: Option<u16>,

    /// Don't open browser automatically
    #[arg(long)]
    no_open: bool,
}

struct AppState {
    trace_file: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Verify trace file exists
    if !args.trace_file.exists() {
        anyhow::bail!("Trace file not found: {}", args.trace_file.display());
    }

    let state = Arc::new(AppState {
        trace_file: args.trace_file,
    });

    let app = Router::new()
        .route("/", get(serve_viewer))
        .route("/events", get(serve_events))
        .with_state(state);

    // Bind to port (auto-select if not specified)
    let port = args.port.unwrap_or(0);
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
    let addr = listener.local_addr()?;

    println!("Serving trace viewer at http://{}", addr);

    // Open browser unless --no-open
    if !args.no_open {
        let url = format!("http://{}", addr);
        if let Err(e) = open::that(&url) {
            eprintln!("Failed to open browser: {}. Open {} manually.", e, url);
        }
    }

    axum::serve(listener, app).await?;

    Ok(())
}

/// Serve the main viewer HTML page
async fn serve_viewer() -> Html<&'static str> {
    Html(include_str!("viewer.html"))
}

/// Serve the trace events as JSON
async fn serve_events(State(state): State<Arc<AppState>>) -> Response {
    match tokio::fs::read_to_string(&state.trace_file).await {
        Ok(content) => {
            // Parse JSONS (newline-delimited JSON) into a JSON array
            let events: Vec<serde_json::Value> = content
                .lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();

            match serde_json::to_string(&events) {
                Ok(json) => {
                    (StatusCode::OK, [("content-type", "application/json")], json).into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to serialize events: {}", e),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read trace file: {}", e),
        )
            .into_response(),
    }
}
