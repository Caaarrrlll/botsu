//! Discord adapter, built on serenity.
//!
//! This is the only crate allowed to touch serenity types.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use botsu_core::voice::{AudioInput, VoiceAdapter};
use botsu_core::{
    Author, BanInfo, Bot, Capabilities, ChatAdapter, Error, HistoryMessage, IncomingMessage,
    Platform, RoleInfo,
};
use serenity::all::{
    ChannelId, ChannelType, Client, Context, CreateAttachment, CreateEmbed, CreateEmbedAuthor,
    CreateEmbedFooter, CreateMessage, EditMember, EventHandler, GatewayIntents, GetMessages, Guild,
    GuildId, Http, Message, MessageId, Ready, RoleId, Timestamp, UserId, VoiceState,
};
use serenity::async_trait;
use songbird::input::core::io::ReadOnlySource;
use songbird::input::{ChildContainer, Input, RawAdapter};
use songbird::tracks::{PlayMode, Track, TrackHandle};
use songbird::{SerenityInit, Songbird};

#[derive(Debug, Clone)]
pub struct DiscordConfig {
    pub token: String,
    /// The invite link requests Administrator instead of the specific permissions.
    pub invite_admin: bool,
}

/// Outbound side: implements the core's [`ChatAdapter`] with Discord REST calls.
pub struct DiscordChat {
    http: Arc<Http>,
}

impl DiscordChat {
    pub fn new(http: Arc<Http>) -> Self {
        Self { http }
    }
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::platform(Platform::Discord, e)
}

/// Discord IDs are u64 snowflakes; the core passes them around as strings.
fn snowflake(id: &str) -> botsu_core::Result<u64> {
    id.parse::<u64>()
        .ok()
        .filter(|&n| n != 0)
        .ok_or_else(|| err(format!("invalid Discord ID {id:?}")))
}

#[async_trait]
impl ChatAdapter for DiscordChat {
    fn platform(&self) -> Platform {
        Platform::Discord
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            bulk_delete: true,
            // Possible later; the core uses prefix commands on every platform for now.
            slash_commands: false,
            // songbird (DiscordVoice); the music queue comes in phase 6.
            voice: true,
        }
    }

    async fn send_message(&self, channel_id: &str, content: &str) -> botsu_core::Result<String> {
        let msg = ChannelId::new(snowflake(channel_id)?)
            .say(&self.http, content)
            .await
            .map_err(err)?;
        Ok(msg.id.to_string())
    }

    async fn send_file(
        &self,
        channel_id: &str,
        content: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> botsu_core::Result<String> {
        let mut msg = CreateMessage::new().add_file(CreateAttachment::bytes(data, filename));
        if !content.is_empty() {
            msg = msg.content(content);
        }
        let sent = ChannelId::new(snowflake(channel_id)?)
            .send_message(&self.http, msg)
            .await
            .map_err(err)?;
        Ok(sent.id.to_string())
    }

    async fn text_channels(
        &self,
        guild_id: &str,
    ) -> botsu_core::Result<Vec<botsu_core::ChannelInfo>> {
        let channels = GuildId::new(snowflake(guild_id)?)
            .channels(&self.http)
            .await
            .map_err(err)?;
        let mut out: Vec<botsu_core::ChannelInfo> = channels
            .values()
            .filter(|c| matches!(c.kind, ChannelType::Text | ChannelType::News))
            .map(|c| botsu_core::ChannelInfo {
                id: c.id.to_string(),
                name: c.name.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn send_embed(
        &self,
        channel_id: &str,
        content: &str,
        embed: &botsu_core::Embed,
    ) -> botsu_core::Result<String> {
        let mut e = CreateEmbed::new().title(&embed.title).url(&embed.url);
        if let Some(a) = &embed.author {
            e = e.author(CreateEmbedAuthor::new(a));
        }
        if let Some(d) = &embed.description {
            e = e.description(d);
        }
        if let Some(i) = &embed.image {
            e = e.image(i);
        }
        if let Some(c) = embed.color {
            e = e.colour(c);
        }
        if let Some(f) = &embed.footer {
            e = e.footer(CreateEmbedFooter::new(f));
        }
        let mut msg = CreateMessage::new().embed(e);
        if !content.is_empty() {
            msg = msg.content(content);
        }
        let sent = ChannelId::new(snowflake(channel_id)?)
            .send_message(&self.http, msg)
            .await
            .map_err(err)?;
        Ok(sent.id.to_string())
    }

    async fn delete_message(&self, channel_id: &str, message_id: &str) -> botsu_core::Result<()> {
        ChannelId::new(snowflake(channel_id)?)
            .delete_message(&self.http, MessageId::new(snowflake(message_id)?))
            .await
            .map_err(err)
    }

    async fn channel_guild(&self, channel_id: &str) -> botsu_core::Result<Option<String>> {
        let channel = ChannelId::new(snowflake(channel_id)?)
            .to_channel(&self.http)
            .await
            .map_err(err)?;
        Ok(channel.guild().map(|c| c.guild_id.to_string()))
    }

    async fn member_roles(&self, guild_id: &str, user_id: &str) -> botsu_core::Result<Vec<String>> {
        let member = GuildId::new(snowflake(guild_id)?)
            .member(&self.http, UserId::new(snowflake(user_id)?))
            .await
            .map_err(err)?;
        Ok(member.roles.iter().map(|r| r.to_string()).collect())
    }

    async fn roles(&self, guild_id: &str) -> botsu_core::Result<Vec<RoleInfo>> {
        let roles = GuildId::new(snowflake(guild_id)?)
            .roles(&self.http)
            .await
            .map_err(err)?;
        Ok(roles
            .values()
            .map(|r| RoleInfo {
                id: r.id.to_string(),
                name: r.name.clone(),
                permissions: r.permissions.bits(),
                position: i64::from(r.position),
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
            .add_member_role(
                GuildId::new(snowflake(guild_id)?),
                UserId::new(snowflake(user_id)?),
                RoleId::new(snowflake(role_id)?),
                None,
            )
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
            .remove_member_role(
                GuildId::new(snowflake(guild_id)?),
                UserId::new(snowflake(user_id)?),
                RoleId::new(snowflake(role_id)?),
                None,
            )
            .await
            .map_err(err)
    }

    async fn recent_messages(
        &self,
        channel_id: &str,
        before: Option<&str>,
        limit: u8,
    ) -> botsu_core::Result<Vec<HistoryMessage>> {
        let mut query = GetMessages::new().limit(limit.min(100));
        if let Some(b) = before {
            query = query.before(MessageId::new(snowflake(b)?));
        }
        let messages = ChannelId::new(snowflake(channel_id)?)
            .messages(&self.http, query)
            .await
            .map_err(err)?;
        Ok(messages
            .into_iter()
            .map(|m| HistoryMessage {
                id: m.id.to_string(),
                author_id: m.author.id.to_string(),
                author_is_bot: m.author.bot,
                timestamp: m.timestamp.unix_timestamp(),
                content: m.content,
            })
            .collect())
    }

    async fn send_dm(&self, user_id: &str, content: &str) -> botsu_core::Result<()> {
        let channel = UserId::new(snowflake(user_id)?)
            .create_dm_channel(&self.http)
            .await
            .map_err(err)?;
        channel.id.say(&self.http, content).await.map_err(err)?;
        Ok(())
    }

    async fn guild_name(&self, guild_id: &str) -> botsu_core::Result<String> {
        let guild = GuildId::new(snowflake(guild_id)?)
            .to_partial_guild(&self.http)
            .await
            .map_err(err)?;
        Ok(guild.name)
    }

    async fn guild_owner(&self, guild_id: &str) -> botsu_core::Result<Option<String>> {
        let guild = GuildId::new(snowflake(guild_id)?)
            .to_partial_guild(&self.http)
            .await
            .map_err(err)?;
        Ok(Some(guild.owner_id.to_string()))
    }

    async fn find_channel(&self, guild_id: &str, name: &str) -> botsu_core::Result<Option<String>> {
        let channels = GuildId::new(snowflake(guild_id)?)
            .channels(&self.http)
            .await
            .map_err(err)?;
        Ok(channels
            .values()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .map(|c| c.id.to_string()))
    }

    /// Discord only bulk-deletes messages younger than 14 days, 2–100 per call.
    /// Callers (phase 4 cleanup) are responsible for chunking and age filtering.
    async fn bulk_delete(
        &self,
        channel_id: &str,
        message_ids: &[String],
    ) -> botsu_core::Result<()> {
        let ids = message_ids
            .iter()
            .map(|id| snowflake(id).map(MessageId::new))
            .collect::<botsu_core::Result<Vec<_>>>()?;
        ChannelId::new(snowflake(channel_id)?)
            .delete_messages(&self.http, ids)
            .await
            .map_err(err)
    }

    async fn bot_user_id(&self) -> botsu_core::Result<String> {
        let me = self.http.get_current_user().await.map_err(err)?;
        Ok(me.id.to_string())
    }

    async fn user_name(&self, user_id: &str) -> botsu_core::Result<String> {
        let user = UserId::new(snowflake(user_id)?)
            .to_user(&self.http)
            .await
            .map_err(err)?;
        Ok(user.global_name.clone().unwrap_or(user.name))
    }

    async fn kick(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> botsu_core::Result<()> {
        self.http
            .kick_member(
                GuildId::new(snowflake(guild_id)?),
                UserId::new(snowflake(user_id)?),
                reason,
            )
            .await
            .map_err(err)
    }

    async fn ban(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
        delete_message_days: u8,
    ) -> botsu_core::Result<()> {
        self.http
            .ban_user(
                GuildId::new(snowflake(guild_id)?),
                UserId::new(snowflake(user_id)?),
                delete_message_days.min(7),
                reason,
            )
            .await
            .map_err(err)
    }

    async fn unban(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> botsu_core::Result<()> {
        self.http
            .remove_ban(
                GuildId::new(snowflake(guild_id)?),
                UserId::new(snowflake(user_id)?),
                reason,
            )
            .await
            .map_err(err)
    }

    async fn bans(&self, guild_id: &str) -> botsu_core::Result<Vec<BanInfo>> {
        let bans = GuildId::new(snowflake(guild_id)?)
            .bans(&self.http, None, None)
            .await
            .map_err(err)?;
        Ok(bans
            .into_iter()
            .map(|b| BanInfo {
                user_id: b.user.id.to_string(),
                user_name: b.user.global_name.clone().unwrap_or(b.user.name),
                reason: b.reason,
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
        let mut edit = match until {
            Some(t) => EditMember::new().disable_communication_until_datetime(
                Timestamp::from_unix_timestamp(t).map_err(err)?,
            ),
            None => EditMember::new().enable_communication(),
        };
        if let Some(r) = reason {
            edit = edit.audit_log_reason(r);
        }
        GuildId::new(snowflake(guild_id)?)
            .edit_member(&self.http, UserId::new(snowflake(user_id)?), edit)
            .await
            .map(|_| ())
            .map_err(err)
    }
}

// --- Voice (songbird) ---------------------------------------------------------------------------

/// Discord voice through songbird (which handles DAVE end-to-end encryption). Who is in which
/// voice channel is tracked from GUILD_CREATE snapshots and VOICE_STATE_UPDATE events, so the
/// memory-hungry serenity cache isn't needed.
pub struct DiscordVoice {
    songbird: Arc<Songbird>,
    /// (guild, user) → voice channel.
    states: Mutex<HashMap<(u64, u64), u64>>,
    /// guild → the track playing (or last played) there, for the idle timer.
    tracks: Mutex<HashMap<u64, TrackHandle>>,
    /// guild → volume (1.0 = unchanged) for tracks started there.
    volumes: Mutex<HashMap<u64, f32>>,
    /// REST client, set once connected: asks Discord directly when the gateway's voice states
    /// don't know where a user is.
    http: std::sync::OnceLock<Arc<Http>>,
}

impl DiscordVoice {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            songbird: Songbird::serenity(),
            states: Mutex::new(HashMap::new()),
            tracks: Mutex::new(HashMap::new()),
            volumes: Mutex::new(HashMap::new()),
            http: std::sync::OnceLock::new(),
        })
    }

    fn states(&self) -> std::sync::MutexGuard<'_, HashMap<(u64, u64), u64>> {
        self.states.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn tracks(&self) -> std::sync::MutexGuard<'_, HashMap<u64, TrackHandle>> {
        self.tracks.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn volume(&self, guild: u64) -> f32 {
        self.volumes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&guild)
            .copied()
            .unwrap_or(1.0)
    }

    fn set_state(&self, guild: GuildId, user: UserId, channel: Option<ChannelId>) {
        let key = (guild.get(), user.get());
        match channel {
            Some(c) => self.states().insert(key, c.get()),
            None => self.states().remove(&key),
        };
    }
}

/// ffmpeg decodes anything (file or URL) to 48 kHz stereo f32 PCM, which songbird plays as raw
/// audio, so no extra decoders are compiled in. Network inputs get reconnect flags.
fn ffmpeg_input(input: AudioInput) -> botsu_core::Result<Input> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    let (source, stdin) = match input {
        AudioInput::Path(p) => (p, Stdio::null()),
        AudioInput::Pipe(s) => ("pipe:0".to_owned(), s),
    };
    if source.starts_with("http://") || source.starts_with("https://") {
        cmd.args([
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_delay_max",
            "5",
        ]);
    }
    let child = cmd
        .args([
            "-i", &source, "-f", "f32le", "-ar", "48000", "-ac", "2", "pipe:1",
        ])
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| err(format!("couldn't start ffmpeg: {e}")))?;
    let pcm = ReadOnlySource::new(ChildContainer::from(child));
    Ok(RawAdapter::new(pcm, 48_000, 2).into())
}

#[async_trait]
impl VoiceAdapter for DiscordVoice {
    fn platform(&self) -> Platform {
        Platform::Discord
    }

    async fn user_channel(&self, guild_id: &str, user_id: &str) -> Option<String> {
        let key: (u64, u64) = (guild_id.parse().ok()?, user_id.parse().ok()?);
        if let Some(c) = self.states().get(&key) {
            return Some(c.to_string());
        }
        // Not seen on the gateway (for example already in voice when the bot started): ask.
        let http = self.http.get()?;
        let guild = GuildId::new(snowflake(guild_id).ok()?);
        let user = UserId::new(snowflake(user_id).ok()?);
        match http.get_user_voice_state(guild, user).await {
            Ok(vs) => {
                let channel = vs.channel_id?;
                self.set_state(guild, user, Some(channel));
                Some(channel.to_string())
            }
            Err(e) => {
                tracing::debug!(error = %e, guild_id, user_id, "no voice state from Discord");
                None
            }
        }
    }

    async fn voice_members(&self, guild_id: &str) -> Vec<(String, String)> {
        let Ok(guild) = guild_id.parse::<u64>() else {
            return Vec::new();
        };
        self.states()
            .iter()
            .filter(|((g, _), _)| *g == guild)
            .map(|((_, u), c)| (u.to_string(), c.to_string()))
            .collect()
    }

    async fn join(&self, guild_id: &str, channel_id: &str) -> botsu_core::Result<()> {
        let guild = GuildId::new(snowflake(guild_id)?);
        let channel = ChannelId::new(snowflake(channel_id)?);
        self.songbird
            .join(guild, channel)
            .await
            .map(|_| ())
            .map_err(|e| err(format!("joining voice failed: {e}")))
    }

    async fn play(&self, guild_id: &str, input: AudioInput) -> botsu_core::Result<Option<String>> {
        let call = self
            .songbird
            .get(GuildId::new(snowflake(guild_id)?))
            .ok_or_else(|| Error::Usage("I'm not in a voice channel; join first.".into()))?;
        let input = ffmpeg_input(input)?;
        let track = Track::from(input).volume(self.volume(snowflake(guild_id)?));
        let (track, channel) = {
            let mut call = call.lock().await;
            (call.play_only(track), call.current_channel())
        };
        self.tracks().insert(snowflake(guild_id)?, track.clone());

        // Report what songbird sees after a moment, so "joined but silent" can be told apart
        // from a failing track: playing with an advancing position means audio is being sent.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let state = track
            .get_info()
            .await
            .map_err(|e| err(format!("couldn't read track state: {e}")))?;
        tracing::info!(?channel, playing = ?state.playing, position = ?state.position, "Discord voice track state");
        match state.playing {
            PlayMode::Errored(e) => Err(err(format!("the audio track failed: {e}"))),
            PlayMode::Play => Ok(Some(format!(
                "songbird is sending audio (at {:.1} s). If you hear nothing, check the bot has \
                 **Speak** in this voice channel.",
                state.position.as_secs_f32()
            ))),
            other => Ok(Some(format!(
                "the track already stopped ({other:?}) after {:.1} s: ffmpeg produced little or \
                 no audio.",
                state.position.as_secs_f32()
            ))),
        }
    }

    async fn stop(&self, guild_id: &str) -> botsu_core::Result<()> {
        if let Some(call) = self.songbird.get(GuildId::new(snowflake(guild_id)?)) {
            call.lock().await.stop();
        }
        Ok(())
    }

    async fn leave(&self, guild_id: &str) -> botsu_core::Result<()> {
        // remove() also drops the call; "not in a channel" isn't worth an error here.
        let guild = snowflake(guild_id)?;
        let _ = self.songbird.remove(GuildId::new(guild)).await;
        self.tracks().remove(&guild);
        Ok(())
    }

    async fn is_connected(&self, guild_id: &str) -> bool {
        let Ok(guild) = snowflake(guild_id) else {
            return false;
        };
        match self.songbird.get(GuildId::new(guild)) {
            Some(call) => call.lock().await.current_channel().is_some(),
            None => false,
        }
    }

    async fn is_playing(&self, guild_id: &str) -> bool {
        let Ok(guild) = snowflake(guild_id) else {
            return false;
        };
        let Some(track) = self.tracks().get(&guild).cloned() else {
            return false;
        };
        matches!(
            track.get_info().await.map(|s| s.playing),
            Ok(PlayMode::Play)
        )
    }

    async fn set_volume(&self, guild_id: &str, percent: u16) -> botsu_core::Result<()> {
        let guild = snowflake(guild_id)?;
        let volume = f32::from(percent) / 100.0;
        self.volumes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(guild, volume);
        if let Some(track) = self.tracks().get(&guild).cloned() {
            // Fails only if the track already ended; the next one uses the new volume anyway.
            let _ = track.set_volume(volume);
        }
        Ok(())
    }

    async fn set_paused(&self, guild_id: &str, paused: bool) -> botsu_core::Result<()> {
        let guild = snowflake(guild_id)?;
        let track = self
            .tracks()
            .get(&guild)
            .cloned()
            .ok_or_else(|| Error::Usage("Nothing is playing.".into()))?;
        let result = if paused { track.pause() } else { track.play() };
        result.map_err(|_| Error::Usage("Nothing is playing.".into()))
    }
}

/// Inbound side: receives gateway events and forwards them to the core.
struct Handler {
    bot: Arc<Bot>,
    voice: Arc<DiscordVoice>,
    invite_admin: bool,
}

#[async_trait]
impl EventHandler for Handler {
    async fn guild_create(&self, _ctx: Context, guild: Guild, _is_new: Option<bool>) {
        tracing::debug!(guild = %guild.id, voice_states = guild.voice_states.len(), "Discord GUILD_CREATE");
        let mut states = self.voice.states();
        states.retain(|(g, _), _| *g != guild.id.get());
        for (user, vs) in &guild.voice_states {
            if let Some(channel) = vs.channel_id {
                states.insert((guild.id.get(), user.get()), channel.get());
            }
        }
    }

    async fn voice_state_update(&self, _ctx: Context, _old: Option<VoiceState>, new: VoiceState) {
        if let Some(guild) = new.guild_id {
            self.voice.set_state(guild, new.user_id, new.channel_id);
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        let _ = self.voice.http.set(ctx.http.clone());
        tracing::info!(
            user = %ready.user.name,
            guilds = ready.guilds.len(),
            "connected to Discord"
        );
        tracing::info!(
            "Discord invite link: https://discord.com/oauth2/authorize?client_id={}&scope=bot&permissions={}",
            ready.application.id,
            botsu_core::invite_permissions(self.invite_admin)
        );
    }

    async fn message(&self, ctx: Context, msg: Message) {
        let message = translate(msg);
        // serenity already runs each event handler in its own task.
        let chat: Arc<dyn ChatAdapter> = Arc::new(DiscordChat::new(ctx.http.clone()));
        self.bot.handle_message(chat, message).await;
    }
}

fn translate(msg: Message) -> IncomingMessage {
    IncomingMessage {
        platform: Platform::Discord,
        guild_id: msg.guild_id.map(|g| g.to_string()),
        channel_id: msg.channel_id.to_string(),
        message_id: msg.id.to_string(),
        author: Author {
            id: msg.author.id.to_string(),
            name: msg.author.name.clone(),
            is_bot: msg.author.bot,
        },
        content: msg.content,
        attachments: msg
            .attachments
            .iter()
            .map(|a| botsu_core::Attachment {
                url: a.url.clone(),
                filename: a.filename.clone(),
                size: u64::from(a.size),
            })
            .collect(),
    }
}

/// Connects to Discord and runs until a fatal error. serenity reconnects on
/// transient failures by itself.
pub async fn run(
    config: DiscordConfig,
    bot: Arc<Bot>,
    voice: Arc<DiscordVoice>,
) -> Result<(), Error> {
    // MESSAGE_CONTENT is a privileged intent: enable it in the developer portal
    // (Bot → Privileged Gateway Intents) or prefix commands won't see any text.
    // GUILD_VOICE_STATES (not privileged) tells us who is in which voice channel.
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&config.token, intents)
        .event_handler(Handler {
            bot: bot.clone(),
            voice: voice.clone(),
            invite_admin: config.invite_admin,
        })
        .register_songbird_with(voice.songbird.clone())
        .await
        .map_err(err)?;
    bot.register_chat(Arc::new(DiscordChat::new(client.http.clone())));
    client.start().await.map_err(|e| match e {
        serenity::Error::Gateway(serenity::gateway::GatewayError::DisallowedGatewayIntents) => err(
            "Message Content intent is not enabled for this bot: Discord developer portal → \
                 your application → Bot → Privileged Gateway Intents → Message Content Intent",
        ),
        serenity::Error::Gateway(serenity::gateway::GatewayError::InvalidAuthentication) => {
            err("Discord rejected DISCORD_TOKEN (reset it on the Bot page and update the env file)")
        }
        other => err(other),
    })
}
