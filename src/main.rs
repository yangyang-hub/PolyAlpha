use anyhow::Result;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .init();

    tracing::info!("PolyAlpha starting...");

    // Load configuration
    let settings = pa_core::config::Settings::load()?;
    tracing::info!(chain_id = settings.chain.chain_id, "Configuration loaded");

    // TODO: Phase 0 - Initialize components
    // 1. Database connection (pa-storage)
    // 2. Monitoring server (pa-monitor)
    //
    // TODO: Phase 1 - Core trading loop
    // 3. Market data feed (pa-market-data)
    // 4. Strategy engine (pa-strategy)
    // 5. Execution engine (pa-execution)
    // 6. Risk manager (pa-risk)

    tracing::info!("PolyAlpha initialized successfully");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received, stopping...");

    Ok(())
}
