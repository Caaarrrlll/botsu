//! Fluxer adapter, built on `fluxer-rust` (library name `fluxer`).
//!
//! This is the only crate allowed to touch fluxer-rust types. When a Fluxer or
//! fluxer-rust update breaks something, the fix should stay inside this crate.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use botsu_core::voice::VoiceAdapter;
use botsu_core::{Author, Bot, Capabilities, ChatAdapter, Error, IncomingMessage, Platform};
use fluxer::http::Http;
use fluxer::prelude::{
    Client, Context, EventHandler, FluxerVoiceConnection, GuildSync, Message, PassiveUpdates,
    Ready, VoiceStateUpdate,
};
use tokio::task::AbortHandle;

/// Public instance used when neither `FLUXER_URL` nor `FLUXER_API_URL` is set. Its API
/// URL is discovered too: it has already moved once (api.fluxer.app → fluxer.com/api).
pub const DEFAULT_INSTANCE: &str = "https://fluxer.app";

/// Where to find the Fluxer API.
#[derive(Debug, Clone)]
pub enum FluxerEndpoint {
    /// The public fluxer.app instance (API URL discovered).
    Default,
    /// An instance's web address (for example `https://fluxer.example.com`); the API URL is
    /// discovered from `<instance>/.well-known/fluxer`.
    Instance(String),
    /// An explicit API base including `/v1`; skips discovery.
    ApiUrl(String),
}

#[derive(Debug, Clone)]
pub struct FluxerConfig {
    pub token: String,
    pub endpoint: FluxerEndpoint,
}

/// Outbound side: implements the core's [`ChatAdapter`] with Fluxer REST calls.
pub struct FluxerChat {
    http: Arc<Http>,
}

impl FluxerChat {
    pub fn new(token: &str, api_url: &str) -> Self {
        Self {
            http: Arc::new(Http::new(token, api_url.to_owned())),
        }
    }
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::platform(Platform::Fluxer, e)
}

#[async_trait]
impl ChatAdapter for FluxerChat {
    fn platform(&self) -> Platform {
        Platform::Fluxer
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            bulk_delete: true,
            // fluxer-rust 0.2 has no interactions API; revisit when it does.
            slash_commands: false,
            // LiveKit via fluxer-rust (FluxerVoice); the music queue comes in phase 6.
            voice: true,
        }
    }

    async fn send_message(&self, channel_id: &str, content: &str) -> botsu_core::Result<String> {
        let msg = self
            .http
            .send_message(channel_id, content)
            .await
            .map_err(err)?;
        Ok(msg.id)
    }

    async fn guild_owner(&self, guild_id: &str) -> botsu_core::Result<Option<String>> {
        let guild = self.http.get_guild(guild_id).await.map_err(err)?;
        Ok(guild.owner_id)
    }

    async fn find_channel(&self, guild_id: &str, name: &str) -> botsu_core::Result<Option<String>> {
        let channels = self.http.get_guild_channels(guild_id).await.map_err(err)?;
        Ok(channels
            .into_iter()
            .find(|c| {
                c.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .map(|c| c.id))
    }

    async fn delete_message(&self, channel_id: &str, message_id: &str) -> botsu_core::Result<()> {
        self.http
            .delete_message(channel_id, message_id)
            .await
            .map_err(err)
    }

    async fn bulk_delete(
        &self,
        channel_id: &str,
        message_ids: &[String],
    ) -> botsu_core::Result<()> {
        let ids = message_ids.iter().map(String::as_str).collect();
        self.http
            .bulk_delete_messages(channel_id, ids)
            .await
            .map_err(err)
    }
}

// --- Voice (LiveKit via fluxer-rust) ------------------------------------------------------------

/// Fluxer voice: fluxer-rust joins a LiveKit room and `play_music` streams ffmpeg's PCM into it.
/// Voice channel membership comes from GUILD_SYNC / PASSIVE_UPDATES snapshots and
/// VOICE_STATE_UPDATE events (fluxer-rust's GUILD_CREATE model drops the voice states).
pub struct FluxerVoice {
    /// The gateway context, needed to join/leave; set once connected.
    ctx: std::sync::Mutex<Option<Context>>,
    /// (guild, user) → voice channel.
    states: std::sync::Mutex<HashMap<(String, String), String>>,
    conns: tokio::sync::Mutex<HashMap<String, FluxerVoiceConnection>>,
    playback: std::sync::Mutex<HashMap<String, AbortHandle>>,
}

impl FluxerVoice {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            ctx: Default::default(),
            states: Default::default(),
            conns: Default::default(),
            playback: Default::default(),
        })
    }

    fn states(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), String>> {
        self.states.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn context(&self) -> botsu_core::Result<Context> {
        self.ctx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| err("not connected to Fluxer yet"))
    }

    fn stop_playback(&self, guild_id: &str) {
        let handle = self
            .playback
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(guild_id);
        if let Some(h) = handle {
            h.abort();
        }
    }

    fn snapshot(&self, source: &str, guild: &str, states: &[serde_json::Value]) {
        let mut map = self.states();
        for vs in states {
            if let (Some(user), Some(channel)) = (vs["user_id"].as_str(), vs["channel_id"].as_str())
            {
                map.insert((guild.to_owned(), user.to_owned()), channel.to_owned());
            }
        }
        tracing::debug!(
            source,
            guild,
            count = states.len(),
            "Fluxer voice state snapshot"
        );
    }
}

#[async_trait]
impl VoiceAdapter for FluxerVoice {
    fn platform(&self) -> Platform {
        Platform::Fluxer
    }

    async fn user_channel(&self, guild_id: &str, user_id: &str) -> Option<String> {
        self.states()
            .get(&(guild_id.to_owned(), user_id.to_owned()))
            .cloned()
    }

    async fn voice_members(&self, guild_id: &str) -> Vec<(String, String)> {
        self.states()
            .iter()
            .filter(|((g, _), _)| g == guild_id)
            .map(|((_, u), c)| (u.clone(), c.clone()))
            .collect()
    }

    async fn join(&self, guild_id: &str, channel_id: &str) -> botsu_core::Result<()> {
        let ctx = self.context()?;
        self.stop_playback(guild_id);
        let conn = ctx
            .join_voice(guild_id, channel_id)
            .await
            .map_err(|e| err(format!("joining voice failed: {e}")))?;
        self.conns.lock().await.insert(guild_id.to_owned(), conn);
        Ok(())
    }

    async fn play(&self, guild_id: &str, source: &str) -> botsu_core::Result<Option<String>> {
        let ctx = self.context()?;
        let conns = self.conns.lock().await;
        let conn = conns
            .get(guild_id)
            .ok_or_else(|| Error::Usage("I'm not in a voice channel; join first.".into()))?;
        self.stop_playback(guild_id);
        // fluxer-rust posts ffmpeg errors to a text channel; we have none here, so that post just
        // fails quietly. botsu's own pipeline (phase 6) will report errors properly.
        let handle = conn
            .play_music(source, ctx.http.clone(), String::new())
            .await
            .map_err(|e| err(format!("playing failed: {e}")))?;
        self.playback
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(guild_id.to_owned(), handle);
        Ok(None)
    }

    async fn stop(&self, guild_id: &str) -> botsu_core::Result<()> {
        self.stop_playback(guild_id);
        Ok(())
    }

    async fn leave(&self, guild_id: &str) -> botsu_core::Result<()> {
        self.stop_playback(guild_id);
        self.conns.lock().await.remove(guild_id);
        let ctx = self.context()?;
        ctx.leave_voice(guild_id)
            .await
            .map_err(|e| err(format!("leaving voice failed: {e}")))
    }
}

/// Inbound side: receives gateway events and forwards them to the core.
struct Handler {
    bot: Arc<Bot>,
    chat: Arc<dyn ChatAdapter>,
    voice: Arc<FluxerVoice>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn on_ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(user = %ready.user.username, "connected to Fluxer");
        *self.voice.ctx.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx);
    }

    async fn on_guild_sync(&self, _ctx: Context, sync: GuildSync) {
        self.voice.snapshot(
            "GUILD_SYNC",
            &sync.id,
            &sync.voice_states.unwrap_or_default(),
        );
    }

    async fn on_passive_updates(&self, _ctx: Context, p: PassiveUpdates) {
        self.voice.snapshot(
            "PASSIVE_UPDATES",
            &p.guild_id,
            &p.voice_states.unwrap_or_default(),
        );
    }

    async fn on_voice_state_update(&self, _ctx: Context, v: VoiceStateUpdate) {
        let Some(guild) = v.guild_id else {
            return;
        };
        let mut states = self.voice.states();
        match v.channel_id {
            Some(channel) => states.insert((guild, v.user_id), channel),
            None => states.remove(&(guild, v.user_id)),
        };
    }

    async fn on_message(&self, _ctx: Context, msg: Message) {
        let Some(message) = translate(msg) else {
            return;
        };
        // Don't hold up the gateway loop while a command runs.
        let bot = self.bot.clone();
        let chat = self.chat.clone();
        tokio::spawn(async move { bot.handle_message(chat, message).await });
    }
}

fn translate(msg: Message) -> Option<IncomingMessage> {
    Some(IncomingMessage {
        platform: Platform::Fluxer,
        guild_id: msg.guild_id,
        channel_id: msg.channel_id?,
        message_id: msg.id,
        author: Author {
            id: msg.author.id,
            name: msg.author.username,
            is_bot: msg.author.bot.unwrap_or(false),
        },
        content: msg.content?,
    })
}

/// Connects to Fluxer and runs until a fatal error. fluxer-rust reconnects on
/// transient failures by itself.
pub async fn run(
    config: FluxerConfig,
    bot: Arc<Bot>,
    voice: Arc<FluxerVoice>,
) -> Result<(), Error> {
    let (api_url, webapp) = resolve_endpoints(&config.endpoint).await?;
    tracing::info!(%api_url, "using Fluxer API");

    let fluxer_chat = FluxerChat::new(&config.token, &api_url);
    let bot_id = check_login(&fluxer_chat.http, &api_url).await?;
    // On Fluxer the bot user's ID is the application ID.
    tracing::info!(
        "Fluxer invite link: {webapp}/oauth2/authorize?client_id={bot_id}&scope=bot&permissions={}",
        botsu_core::INVITE_PERMISSIONS
    );

    let chat: Arc<dyn ChatAdapter> = Arc::new(fluxer_chat);
    bot.register_chat(chat.clone());
    let mut client = Client::builder(&config.token)
        .api_url(&api_url)
        .event_handler(Handler { bot, chat, voice })
        .build();
    client.start().await.map_err(err)
}

/// `(API base with /v1, web app address)`.
async fn resolve_endpoints(endpoint: &FluxerEndpoint) -> Result<(String, String), Error> {
    match endpoint {
        FluxerEndpoint::Default => discover(DEFAULT_INSTANCE).await,
        FluxerEndpoint::Instance(origin) => discover(origin).await,
        FluxerEndpoint::ApiUrl(url) => {
            let url = url.trim_end_matches('/').to_owned();
            Ok((url.clone(), site_of(&url)))
        }
    }
}

/// `https://host[:port]/anything` → `https://host[:port]`.
fn site_of(url: &str) -> String {
    let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
    match url[scheme_end..].find('/') {
        Some(i) => url[..scheme_end + i].to_owned(),
        None => url.to_owned(),
    }
}

/// Reads `<origin>/.well-known/fluxer` and returns `endpoints.api_public` + `/v1`.
///
/// We fetch this ourselves rather than via fluxer-rust's `get_well_known_fluxer`:
/// that one looks under the API path (`/api/.well-known/fluxer`), while instances
/// serve it at the domain root, and its typed struct fails on any missing field.
async fn discover(origin: &str) -> Result<(String, String), Error> {
    let origin = origin.trim_end_matches('/');
    let url = format!("{origin}/.well-known/fluxer");
    let fail = |e: &dyn std::fmt::Display| {
        err(format!(
            "could not read {url} ({e}); check FLUXER_URL is your instance's web address, \
             or set FLUXER_API_URL directly"
        ))
    };

    // A token-less client: the discovery document is public.
    let doc: serde_json::Value = Http::new_user("", origin.to_owned())
        .client
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| fail(&e))?
        .json()
        .await
        .map_err(|e| fail(&e))?;

    let endpoints = &doc["endpoints"];
    let base = endpoints["api_public"]
        .as_str()
        .or_else(|| endpoints["api"].as_str())
        .ok_or_else(|| fail(&"no endpoints.api_public in the document"))?
        .trim_end_matches('/');

    let features = &doc["features"];
    tracing::info!(
        instance = origin,
        gateway = endpoints["gateway"].as_str().unwrap_or("?"),
        voice_enabled = features["voice_enabled"].as_bool(),
        self_hosted = features["self_hosted"].as_bool(),
        "Fluxer instance discovered"
    );

    // The web app can live elsewhere than the discovery document (it doesn't on fluxer.app or a
    // default self-hosted setup, but the document says so explicitly).
    let webapp = endpoints["webapp"]
        .as_str()
        .filter(|s| s.starts_with("http"))
        .unwrap_or(origin)
        .trim_end_matches('/')
        .to_owned();
    Ok((with_v1(base), webapp))
}

/// The discovery document publishes the API base without the version segment.
fn with_v1(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        base.to_owned()
    } else {
        format!("{base}/v1")
    }
}

/// Fails fast with a readable error if the token or API URL is wrong, instead
/// of letting the gateway loop retry forever.
/// Returns the bot's user ID.
async fn check_login(http: &Http, api_url: &str) -> Result<String, Error> {
    let me = http.get_me().await.map_err(|e| {
        err(format!(
            "login check failed against {api_url} ({e}); check FLUXER_TOKEN"
        ))
    })?;
    tracing::info!(user = %me.username, id = %me.id, "Fluxer token OK");
    Ok(me.id)
}

#[cfg(test)]
mod tests {
    use super::{site_of, with_v1};

    #[test]
    fn site_of_api_url() {
        assert_eq!(
            site_of("https://fluxer.example.org/api/v1"),
            "https://fluxer.example.org"
        );
        assert_eq!(site_of("http://192.0.2.10:8080"), "http://192.0.2.10:8080");
    }

    #[test]
    fn appends_version_once() {
        assert_eq!(
            with_v1("https://fluxer.com/api"),
            "https://fluxer.com/api/v1"
        );
        assert_eq!(
            with_v1("https://fluxer.com/api/"),
            "https://fluxer.com/api/v1"
        );
        assert_eq!(with_v1("https://x.lan/api/v1"), "https://x.lan/api/v1");
    }
}
