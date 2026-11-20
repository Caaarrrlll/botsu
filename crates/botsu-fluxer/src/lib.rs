//! Fluxer adapter, built on `fluxer-rust` (library name `fluxer`).
//!
//! This is the only crate allowed to touch fluxer-rust types. When a Fluxer or
//! fluxer-rust update breaks something, the fix should stay inside this crate.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use botsu_core::voice::{AudioInput, VoiceAdapter};
use botsu_core::{
    Author, BanInfo, Bot, Capabilities, ChatAdapter, Error, HistoryMessage, IncomingMessage,
    Platform, RoleInfo,
};
use fluxer::http::Http;
use fluxer::prelude::{
    Client, Context, EventHandler, FluxerVoiceConnection, GetMessagesQuery, Guild, GuildSync,
    Message, PassiveUpdates, Ready, VoiceStateUpdate,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
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
    /// The bot's user ID (from the login check).
    bot_id: String,
}

impl FluxerChat {
    pub fn new(http: Arc<Http>, bot_id: String) -> Self {
        Self { http, bot_id }
    }

    /// A REST call with an optional JSON body and audit-log reason. fluxer-rust's moderation
    /// helpers can't send a reason (or ban options), so these go through its client directly.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
        reason: Option<&str>,
    ) -> botsu_core::Result<serde_json::Value> {
        let url = format!("{}{path}", self.http.base_url);
        let client = &self.http.client;
        let mut req = match method {
            Method::Post => client.post(&url),
            Method::Put => client.put(&url),
            Method::Patch => client.patch(&url),
            Method::Delete => client.delete(&url),
        };
        if let Some(r) = reason.filter(|r| !r.trim().is_empty()) {
            req = req.header("X-Audit-Log-Reason", percent_encode(r));
        }
        req = match body {
            Some(b) => req.json(&b),
            None => req.body(""),
        };
        let resp = req.send().await.map_err(err)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(err(format!("HTTP {status}: {text}")));
        }
        // 204 and other empty bodies come back as Null.
        let text = resp.text().await.unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }
}

enum Method {
    Post,
    Put,
    Patch,
    Delete,
}

/// Audit-log reasons travel in a header, URL-encoded.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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

    async fn channel_guild(&self, channel_id: &str) -> botsu_core::Result<Option<String>> {
        let channel = self.http.get_channel(channel_id).await.map_err(err)?;
        Ok(channel.guild_id)
    }

    async fn member_roles(&self, guild_id: &str, user_id: &str) -> botsu_core::Result<Vec<String>> {
        let member = self
            .http
            .get_guild_member(guild_id, user_id)
            .await
            .map_err(err)?;
        Ok(member.roles)
    }

    async fn roles(&self, guild_id: &str) -> botsu_core::Result<Vec<RoleInfo>> {
        let roles = self.http.get_guild_roles(guild_id).await.map_err(err)?;
        Ok(roles
            .into_iter()
            .map(|r| RoleInfo {
                // Fluxer sends permission bits as a decimal string, like Discord.
                permissions: r
                    .permissions
                    .as_deref()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0),
                position: r.position.unwrap_or(0),
                id: r.id,
                name: r.name,
            })
            .collect())
    }

    async fn add_role(
        &self,
        guild_id: &str,
        user_id: &str,
        role_id: &str,
    ) -> botsu_core::Result<()> {
        self.http
            .add_member_role(guild_id, user_id, role_id)
            .await
            .map_err(err)
    }

    async fn remove_role(
        &self,
        guild_id: &str,
        user_id: &str,
        role_id: &str,
    ) -> botsu_core::Result<()> {
        self.http
            .remove_member_role(guild_id, user_id, role_id)
            .await
            .map_err(err)
    }

    async fn recent_messages(
        &self,
        channel_id: &str,
        before: Option<&str>,
        limit: u8,
    ) -> botsu_core::Result<Vec<HistoryMessage>> {
        let query = GetMessagesQuery {
            limit: Some(limit.min(100)),
            before: before.map(str::to_owned),
            after: None,
            around: None,
        };
        let messages = self
            .http
            .get_messages(channel_id, query)
            .await
            .map_err(err)?;
        Ok(messages
            .into_iter()
            .map(|m| HistoryMessage {
                timestamp: m
                    .timestamp
                    .as_deref()
                    .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok())
                    .map(|t| t.unix_timestamp())
                    .unwrap_or(0),
                author_id: m.author.id,
                author_is_bot: m.author.bot.unwrap_or(false),
                content: m.content.unwrap_or_default(),
                id: m.id,
            })
            .collect())
    }

    async fn send_dm(&self, user_id: &str, content: &str) -> botsu_core::Result<()> {
        // fluxer-rust has no DM helper: open (or reuse) the DM channel, then post in it.
        let body = serde_json::json!({ "recipient_id": user_id });
        let channel = self
            .call(Method::Post, "/users/@me/channels", Some(body), None)
            .await?;
        let id = channel["id"]
            .as_str()
            .ok_or_else(|| err("opening a DM returned no channel ID"))?;
        self.send_message(id, content).await.map(|_| ())
    }

    async fn guild_name(&self, guild_id: &str) -> botsu_core::Result<String> {
        let guild = self.http.get_guild(guild_id).await.map_err(err)?;
        Ok(guild.name.unwrap_or_else(|| guild_id.to_owned()))
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

    async fn bot_user_id(&self) -> botsu_core::Result<String> {
        Ok(self.bot_id.clone())
    }

    async fn user_name(&self, user_id: &str) -> botsu_core::Result<String> {
        let user = self.http.get_user(user_id).await.map_err(err)?;
        Ok(user.username)
    }

    async fn kick(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> botsu_core::Result<()> {
        let path = format!("/guilds/{guild_id}/members/{user_id}");
        self.call(Method::Delete, &path, None, reason)
            .await
            .map(|_| ())
    }

    async fn ban(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
        delete_message_days: u8,
    ) -> botsu_core::Result<()> {
        let path = format!("/guilds/{guild_id}/bans/{user_id}");
        let body = serde_json::json!({
            "reason": reason,
            "delete_message_days": delete_message_days.min(7),
        });
        self.call(Method::Put, &path, Some(body), reason)
            .await
            .map(|_| ())
    }

    async fn unban(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> botsu_core::Result<()> {
        let path = format!("/guilds/{guild_id}/bans/{user_id}");
        self.call(Method::Delete, &path, None, reason)
            .await
            .map(|_| ())
    }

    async fn bans(&self, guild_id: &str) -> botsu_core::Result<Vec<BanInfo>> {
        let bans = self.http.get_guild_bans(guild_id).await.map_err(err)?;
        Ok(bans
            .iter()
            .filter_map(|b| {
                let user = &b["user"];
                let id = user["id"].as_str()?.to_owned();
                Some(BanInfo {
                    user_name: user["global_name"]
                        .as_str()
                        .or_else(|| user["username"].as_str())
                        .unwrap_or(&id)
                        .to_owned(),
                    reason: b["reason"].as_str().map(str::to_owned),
                    user_id: id,
                })
            })
            .collect())
    }

    async fn timeout(
        &self,
        guild_id: &str,
        user_id: &str,
        until: Option<i64>,
        reason: Option<&str>,
    ) -> botsu_core::Result<()> {
        let until = until
            .map(|t| {
                OffsetDateTime::from_unix_timestamp(t)
                    .ok()
                    .and_then(|t| t.format(&Rfc3339).ok())
                    .ok_or_else(|| err(format!("invalid time {t}")))
            })
            .transpose()?;
        let mut body = serde_json::json!({ "communication_disabled_until": until });
        if let (Some(r), Some(_)) = (reason, &until) {
            body["timeout_reason"] = r.into();
        }
        let path = format!("/guilds/{guild_id}/members/{user_id}");
        self.call(Method::Patch, &path, Some(body), reason)
            .await
            .map(|_| ())
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
    playback: std::sync::Mutex<HashMap<String, Playback>>,
    /// guild → volume in percent, shared with that server's player (it scales every frame).
    volumes: std::sync::Mutex<HashMap<String, Arc<AtomicU16>>>,
}

/// One server's running playback task and its pause flag.
struct Playback {
    task: AbortHandle,
    paused: Arc<AtomicBool>,
}

impl FluxerVoice {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            ctx: Default::default(),
            states: Default::default(),
            conns: Default::default(),
            playback: Default::default(),
            volumes: Default::default(),
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

    fn volume(&self, guild_id: &str) -> Arc<AtomicU16> {
        self.volumes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(guild_id.to_owned())
            .or_insert_with(|| Arc::new(AtomicU16::new(100)))
            .clone()
    }

    fn stop_playback(&self, guild_id: &str) {
        let handle = self
            .playback
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(guild_id);
        if let Some(p) = handle {
            p.task.abort();
        }
    }

    /// Records voice states from a guild payload. `full`: the payload lists everyone in voice
    /// (GUILD_CREATE, GUILD_SYNC), so earlier entries for that server are dropped first.
    fn snapshot(&self, source: &str, guild: &str, states: &[serde_json::Value], full: bool) {
        let mut map = self.states();
        if full {
            map.retain(|(g, _), _| g != guild);
        }
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

    async fn play(&self, guild_id: &str, input: AudioInput) -> botsu_core::Result<Option<String>> {
        let (source, stdin) = match input {
            AudioInput::Path(p) => (p, None),
            AudioInput::Pipe(s) => ("pipe:0".to_owned(), Some(s)),
        };
        self.stop_playback(guild_id);
        let paused = Arc::new(AtomicBool::new(false));
        let volume = self.volume(guild_id);
        let mut task = {
            let conns = self.conns.lock().await;
            let conn = conns
                .get(guild_id)
                .ok_or_else(|| Error::Usage("I'm not in a voice channel; join first.".into()))?;
            // botsu's patched player (vendor/fluxer-rust): reconnect flags, stdin input, errors
            // returned instead of posted to a channel, pausing, volume.
            conn.play_input(&source, stdin, paused.clone(), volume)
                .map_err(|e| err(format!("playing failed: {e}")))?
        };
        self.playback
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                guild_id.to_owned(),
                Playback {
                    task: task.abort_handle(),
                    paused,
                },
            );
        // An input that fails right away (bad link, download refused) fails here.
        tokio::select! {
            done = &mut task => {
                return match done {
                    Ok(Ok(())) => Ok(None),
                    Ok(Err(e)) => Err(err(e)),
                    Err(e) if e.is_cancelled() => Ok(None),
                    Err(e) => Err(err(e)),
                };
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
        }
        let guild = guild_id.to_owned();
        tokio::spawn(async move {
            match task.await {
                Ok(Err(e)) => tracing::warn!(guild, error = %e, "Fluxer playback failed"),
                Err(e) if !e.is_cancelled() => {
                    tracing::warn!(guild, error = %e, "Fluxer playback panicked")
                }
                _ => {}
            }
        });
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

    async fn is_connected(&self, guild_id: &str) -> bool {
        self.conns.lock().await.contains_key(guild_id)
    }

    async fn is_playing(&self, guild_id: &str) -> bool {
        self.playback
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(guild_id)
            .is_some_and(|p| !p.task.is_finished() && !p.paused.load(Ordering::Relaxed))
    }

    async fn set_volume(&self, guild_id: &str, percent: u16) -> botsu_core::Result<()> {
        // The player reads it for every 20 ms frame, so this applies right away.
        self.volume(guild_id).store(percent, Ordering::Relaxed);
        Ok(())
    }

    async fn set_paused(&self, guild_id: &str, paused: bool) -> botsu_core::Result<()> {
        let playback = self.playback.lock().unwrap_or_else(|p| p.into_inner());
        match playback.get(guild_id) {
            Some(p) if !p.task.is_finished() => {
                p.paused.store(paused, Ordering::Relaxed);
                Ok(())
            }
            _ => Err(Error::Usage("Nothing is playing.".into())),
        }
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

    async fn on_guild_create(&self, _ctx: Context, guild: Guild) {
        // voice_states comes from botsu's fluxer-rust patch (vendor/README.md).
        self.voice.snapshot(
            "GUILD_CREATE",
            &guild.id,
            &guild.voice_states.unwrap_or_default(),
            true,
        );
    }

    async fn on_guild_sync(&self, _ctx: Context, sync: GuildSync) {
        self.voice.snapshot(
            "GUILD_SYNC",
            &sync.id,
            &sync.voice_states.unwrap_or_default(),
            true,
        );
    }

    async fn on_passive_updates(&self, _ctx: Context, p: PassiveUpdates) {
        self.voice.snapshot(
            "PASSIVE_UPDATES",
            &p.guild_id,
            &p.voice_states.unwrap_or_default(),
            false,
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
    let (http, bot_id, webapp) = connect_rest(&config).await?;
    let api_url = http.base_url.clone();
    // On Fluxer the bot user's ID is the application ID.
    tracing::info!(
        "Fluxer invite link: {webapp}/oauth2/authorize?client_id={bot_id}&scope=bot&permissions={}",
        botsu_core::INVITE_PERMISSIONS
    );

    let chat: Arc<dyn ChatAdapter> = Arc::new(FluxerChat::new(http, bot_id));
    bot.register_chat(chat.clone());
    let mut client = Client::builder(&config.token)
        .api_url(&api_url)
        .event_handler(Handler { bot, chat, voice })
        .build();
    client.start().await.map_err(err)
}

/// Finds the API, checks the token and returns `(REST client, bot user ID, web app address)`.
/// Also used by the API spike (`examples/api_spike.rs`).
pub async fn connect_rest(config: &FluxerConfig) -> Result<(Arc<Http>, String, String), Error> {
    let (api_url, webapp) = resolve_endpoints(&config.endpoint).await?;
    tracing::info!(%api_url, "using Fluxer API");
    let http = Arc::new(Http::new(&config.token, api_url.clone()));
    let bot_id = check_login(&http, &api_url).await?;
    Ok((http, bot_id, webapp))
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
        assert_eq!(site_of("http://10.0.0.5:8080"), "http://10.0.0.5:8080");
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
