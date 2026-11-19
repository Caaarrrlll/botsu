mod config;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use botsu_core::alerts::{self, twitch::Twitch, youtube::YouTubeApi, TubeCommand, TwitchCommand};
use botsu_core::storage::Store;
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
    tracing::info!(
        prefix = %config.prefix,
        data_dir = %config.data_dir.display(),
        "starting botsu {}",
        env!("CARGO_PKG_VERSION")
    );

    let store = Arc::new(Store::open(&config.data_dir).map_err(anyhow::Error::msg)?);
    let http = reqwest::Client::builder()
        .user_agent(concat!("botsu/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?;

    let youtube_api = match config.youtube_api_key {
        Some(key) => {
            let api = Arc::new(YouTubeApi::new(http.clone(), key));
            api.check()
                .await
                .map_err(|e| anyhow::anyhow!("YOUTUBE_API_KEY doesn't work: {e}"))?;
            tracing::info!("YouTube Data API enabled (@handles, live/upcoming detection)");
            Some(api)
        }
        None => {
            tracing::warn!(
                "YOUTUBE_API_KEY not set: YouTube alerts work, but only with channel IDs/links, \
                 and live streams are posted as ordinary uploads"
            );
            None
        }
    };

    let twitch = match config.twitch {
        Some((id, secret)) => {
            let client = Arc::new(Twitch::new(http.clone(), id, secret));
            // Fail at startup, not on the first `twitch add`, if the credentials are wrong.
            client.check().await.map_err(anyhow::Error::msg)?;
            tracing::info!("Twitch alerts enabled");
            Some(client)
        }
        None => {
            tracing::info!(
                "Twitch alerts disabled (TWITCH_CLIENT_ID / TWITCH_CLIENT_SECRET not set)"
            );
            None
        }
    };

    let mut bot = Bot::new(config.prefix.clone(), config.owners);
    if !bot.has_owners() {
        tracing::warn!("BOTSU_OWNERS is empty: nobody can add or remove alerts. Run an admin command to see your user ID.");
    }
    bot.register(TubeCommand::new(
        store.clone(),
        http.clone(),
        youtube_api.clone(),
    ));
    bot.register(TwitchCommand::new(store.clone(), twitch.clone()));
    let bot = Arc::new(bot);

    // One task per platform. If any adapter dies, exit so Docker restarts us.
    let mut adapters = JoinSet::new();
    if let Some(discord) = config.discord {
        let bot = bot.clone();
        adapters.spawn(async move { ("discord", botsu_discord::run(discord, bot).await) });
    }
    if let Some(fluxer) = config.fluxer {
        let bot = bot.clone();
        adapters.spawn(async move { ("fluxer", botsu_fluxer::run(fluxer, bot).await) });
    }

    alerts::spawn_pollers(bot.clone(), store, http, youtube_api, twitch, config.alerts);

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
