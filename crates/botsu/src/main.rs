mod config;
mod logging;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use botsu_core::alerts::{self, twitch::Twitch, youtube::YouTubeApi, TubeCommand, TwitchCommand};
use botsu_core::moderation::{self, ModTools};
use botsu_core::music::{self, Music};
use botsu_core::permissions::Permissions;
use botsu_core::server::{self, SetCommand};
use botsu_core::storage::Store;
use botsu_core::voice::VoiceAdapter;
use botsu_core::ytdlp::{self, YtDlp};
use botsu_core::Bot;
use botsu_discord::DiscordVoice;
use botsu_fluxer::FluxerVoice;

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();

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
    bot.enable_server_prefixes(store.clone());
    bot.register(SetCommand::new(store.clone()));
    bot.register(server::admin_group(
        store.clone(),
        http.clone(),
        twitch.is_some(),
    ));
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
    let ytdlp = YtDlp::new(&config.data_dir, config.ytdlp.clone(), http.clone());
    ytdlp::spawn_maintenance(ytdlp.clone());
    bot.register(music::group(&Music::new(voices, ytdlp, store.clone())));
    // Last: custom commands can't take the names registered above.
    bot.enable_custom_commands(store.clone());
    let bot = Arc::new(bot);

    // One task per platform, each started again after a failure, so a platform that's down
    // (for example a self-hosted Fluxer behind a proxy answering 502) never takes the others
    // down with it.
    if let (Some(discord), Some(voice)) = (config.discord, discord_voice) {
        let bot = bot.clone();
        tokio::spawn(keep_running("discord", move || {
            botsu_discord::run(discord.clone(), bot.clone(), voice.clone())
        }));
    }
    if let (Some(fluxer), Some(voice)) = (config.fluxer, fluxer_voice) {
        let bot = bot.clone();
        tokio::spawn(keep_running("fluxer", move || {
            botsu_fluxer::run(fluxer.clone(), bot.clone(), voice.clone())
        }));
    }

    moderation::spawn_unban_scheduler(bot.clone(), mod_tools);
    alerts::spawn_pollers(bot.clone(), store, http, youtube_api, twitch, config.alerts);

    shutdown_signal().await;
    tracing::info!("shutting down");
    Ok(())
}

/// First wait before starting a failed platform again, and the longest wait.
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(10 * 60);

/// Runs one platform for as long as the bot runs. When it stops, it's started again after a
/// wait that doubles each time (30 s up to 10 min), back to 30 s once it had run for 10 minutes.
async fn keep_running<F, Fut, E>(name: &'static str, mut start: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<(), E>>,
    E: std::fmt::Display,
{
    let mut wait = RETRY_FIRST;
    loop {
        let started = tokio::time::Instant::now();
        match start().await {
            // fluxer-rust returns Ok after an invalid-token close (4004).
            Ok(()) => tracing::error!(platform = name, "stopped (check its token)"),
            Err(e) => tracing::error!(platform = name, error = %e, "failed"),
        }
        if started.elapsed() >= RETRY_MAX {
            wait = RETRY_FIRST;
        }
        tracing::warn!(
            platform = name,
            retry_in_s = wait.as_secs(),
            "starting it again later; the other platforms keep running"
        );
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RETRY_MAX);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A platform that fails twice and then stays up: started three times, with 30 s and then
    /// 60 s in between, and nothing else is stopped by the failures.
    #[tokio::test(start_paused = true)]
    async fn failed_platform_is_started_again_with_growing_waits() {
        let starts = Arc::new(AtomicU32::new(0));
        let counter = starts.clone();
        let task = tokio::spawn(keep_running("test", move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err("502 Bad Gateway")
                } else {
                    std::future::pending::<std::result::Result<(), &str>>().await
                }
            }
        }));

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 2, "first retry after 30 s");
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 2, "second wait is 60 s");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 3);
        assert!(!task.is_finished(), "keeps running");
        task.abort();
    }
}
