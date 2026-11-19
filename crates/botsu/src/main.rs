mod config;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use botsu_core::alerts::{self, twitch::Twitch, youtube::YouTubeApi, TubeCommand, TwitchCommand};
use botsu_core::moderation::{self, ModTools};
use botsu_core::permissions::Permissions;
use botsu_core::server::{self, SetCommand};
use botsu_core::storage::Store;
use botsu_core::voice::{MusicCommand, VoiceAdapter};
use botsu_core::Bot;
use botsu_discord::DiscordVoice;
use botsu_fluxer::FluxerVoice;
use tokio::task::JoinSet;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            // A blank RUST_LOG (an empty Unraid template field) means "default", not "errors only".
            std::env::var("RUST_LOG")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .and_then(|v| EnvFilter::try_new(v).ok())
                .unwrap_or_else(|| EnvFilter::new("info,botsu=debug")),
        )
        .init();

    let config = load_config().await?;
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
    let permissions = Arc::new(Permissions::new(store.clone()));
    bot.set_permissions(permissions.clone());
    bot.register(SetCommand::new(store.clone()));
    bot.register(server::admin_group());
    let mod_tools = ModTools::new(store.clone(), permissions);
    bot.register(moderation::group(&mod_tools));
    if !bot.has_owners() {
        tracing::warn!("BOTSU_OWNERS is empty: nobody can run tube/twitch import or tube shorts. Run one to see your user ID.");
    }
    bot.register(TubeCommand::new(
        store.clone(),
        http.clone(),
        youtube_api.clone(),
    ));
    bot.register(TwitchCommand::new(store.clone(), twitch.clone()));

    // Voice adapters exist up front so the `audio` command can hold them; the platform tasks
    // below fill in their connections and voice states.
    let discord_voice = config.discord.as_ref().map(|_| DiscordVoice::new());
    let fluxer_voice = config.fluxer.as_ref().map(|_| FluxerVoice::new());
    let mut voices: Vec<Arc<dyn VoiceAdapter>> = Vec::new();
    if let Some(v) = &discord_voice {
        voices.push(v.clone());
    }
    if let Some(v) = &fluxer_voice {
        voices.push(v.clone());
    }
    bot.register(MusicCommand::new(voices));
    let bot = Arc::new(bot);

    // One task per platform. If any adapter dies, exit so Docker restarts us.
    let mut adapters = JoinSet::new();
    if let (Some(discord), Some(voice)) = (config.discord, discord_voice) {
        let bot = bot.clone();
        adapters.spawn(async move { ("discord", botsu_discord::run(discord, bot, voice).await) });
    }
    if let (Some(fluxer), Some(voice)) = (config.fluxer, fluxer_voice) {
        let bot = bot.clone();
        adapters.spawn(async move { ("fluxer", botsu_fluxer::run(fluxer, bot, voice).await) });
    }

    moderation::spawn_unban_scheduler(bot.clone(), mod_tools);
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

/// Loads the configuration. With no platform token anywhere yet, makes sure the settings file
/// exists (creating a commented template) and waits for it to be filled in, so a fresh
/// container needs no files prepared in advance.
async fn load_config() -> Result<config::Config> {
    let data_dir = config::data_dir();
    let settings = data_dir.join(config::SETTINGS_FILE);
    let mut told = false;
    loop {
        let vars = config::Vars::new(config::read_settings(&settings)?);
        if let Some(cfg) = config::Config::load(&vars, data_dir.clone())? {
            return Ok(cfg);
        }
        if !told {
            if !settings.exists() {
                config::write_settings_template(&settings)?;
                tracing::warn!(file = %settings.display(), "created a settings file");
            }
            tracing::warn!(
                file = %settings.display(),
                "no platform configured: set DISCORD_TOKEN and/or FLUXER_TOKEN in this file, \
                 or as environment variables (Unraid: the template's token fields, then Apply). \
                 Checking the file again every 30 seconds"
            );
            told = true;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = shutdown_signal() => std::process::exit(0),
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
