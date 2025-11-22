//! Debug logging for conductor

use chrono::Local;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// A debug logger that writes lines to a timestamped log file
pub struct DebugLogger {
    writer: Arc<Mutex<tokio::io::BufWriter<tokio::fs::File>>>,
}

impl DebugLogger {
    /// Create a new debug logger with a timestamped log file
    pub async fn new(
        debug_dir: Option<PathBuf>,
        component_commands: &[String],
    ) -> Result<Self, std::io::Error> {
        // Create log directory
        let log_dir = debug_dir.unwrap_or_else(|| PathBuf::from("."));
        tokio::fs::create_dir_all(&log_dir).await?;

        // Create timestamped log file
        let timestamp = Local::now().format("%Y%m%d-%H%M%S");
        let log_file = log_dir.join(format!("{}.log", timestamp));

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .await?;

        let mut writer = tokio::io::BufWriter::new(file);

        // Write header
        writer.write_all(b"=== Conductor Debug Log ===\n").await?;
        writer
            .write_all(format!("Started: {}\n", Local::now().to_rfc3339()).as_bytes())
            .await?;
        writer.write_all(b"Components:\n").await?;
        for (i, cmd) in component_commands.iter().enumerate() {
            writer
                .write_all(format!("  {}: {}\n", i, cmd).as_bytes())
                .await?;
        }
        writer.write_all(b"========================\n").await?;
        writer.flush().await?;

        eprintln!("Debug logging to: {}", log_file.display());

        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
        })
    }

    /// Create a callback for a specific component
    pub fn create_callback(
        &self,
        component_label: String,
    ) -> impl Fn(&str, sacp_tokio::LineDirection) + Send + Sync + 'static {
        let writer = self.writer.clone();
        move |line: &str, direction: sacp_tokio::LineDirection| {
            let writer = writer.clone();
            let component_label = component_label.clone();
            let line = line.to_string();
            tokio::spawn(async move {
                let arrow = match direction {
                    sacp_tokio::LineDirection::Stdin => "→",
                    sacp_tokio::LineDirection::Stdout => "←",
                    sacp_tokio::LineDirection::Stderr => "!",
                };

                // Strip ANSI escape codes from stderr to keep logs clean
                let cleaned_line = if matches!(direction, sacp_tokio::LineDirection::Stderr) {
                    let bytes = strip_ansi_escapes::strip(&line);
                    String::from_utf8_lossy(&bytes).to_string()
                } else {
                    line
                };

                let log_line = format!("{} {} {}\n", component_label, arrow, cleaned_line);
                let mut writer = writer.lock().await;
                let _ = writer.write_all(log_line.as_bytes()).await;
                let _ = writer.flush().await;
            });
        }
    }
}
