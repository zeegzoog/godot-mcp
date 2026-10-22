use godot_mcp::config::Config;
use godot_mcp::error::Result;
use godot_mcp::server::McpServer;
use std::io::IsTerminal;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Direct all diagnostic logs to stderr to maintain pristine JSON-RPC on stdout
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("godot_mcp=info,warn")),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                // Colour codes only for a person reading a terminal; a client that stores the log
                // gets plain text.
                .with_ansi(std::io::stderr().is_terminal()),
        )
        .init();

    let config = Config::load();
    let server = McpServer::new(&config);

    server.run_stdio().await
}
