mod config;

use std::sync::Arc;

use anyhow::Result;
use botsu_core::Bot;
use tokio::task::JoinSet;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,botsu=debug")),
        )
        .init();

    let config = config::Config::from_env()?;
    let bot = Arc::new(Bot::new(config.prefix.clone()));
    tracing::info!(
        prefix = bot.prefix(),
        "starting botsu {}",
        env!("CARGO_PKG_VERSION")
    );

    // One task per platform. If any adapter dies, exit so Docker restarts us.
    let mut adapters = JoinSet::new();
    if let Some(discord) = config.discord {
        let bot = bot.clone();
        adapters.spawn(async move { ("discord", botsu_discord::run(discord, bot).await) });
    }
    if let Some(fluxer) = config.fluxer {
        tracing::info!(api_url = %fluxer.api_url, "Fluxer enabled");
        let bot = bot.clone();
        adapters.spawn(async move { ("fluxer", botsu_fluxer::run(fluxer, bot).await) });
    }

    tokio::select! {
        Some(done) = adapters.join_next() => {
            let (name, result) = done?;
            match result {
                // fluxer-rust returns Ok after an invalid-token close (4004).
                Ok(()) => anyhow::bail!("{name} adapter stopped (check its token)"),
                Err(e) => anyhow::bail!("{name} adapter failed: {e}"),
            }
        }
        _ = shutdown_signal() => {
            tracing::info!("shutting down");
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
