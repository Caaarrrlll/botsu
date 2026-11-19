//! Discord adapter, built on serenity.
//!
//! This is the only crate allowed to touch serenity types.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use botsu_core::voice::VoiceAdapter;
use botsu_core::{Author, Bot, Capabilities, ChatAdapter, Error, IncomingMessage, Platform};
use serenity::all::{
    ChannelId, Client, Context, EventHandler, GatewayIntents, Guild, GuildId, Http, Message,
    MessageId, Ready, UserId, VoiceState,
};
use serenity::async_trait;
use songbird::input::core::io::ReadOnlySource;
use songbird::input::{ChildContainer, Input, RawAdapter};
use songbird::tracks::PlayMode;
use songbird::{SerenityInit, Songbird};

#[derive(Debug, Clone)]
pub struct DiscordConfig {
    pub token: String,
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

    async fn delete_message(&self, channel_id: &str, message_id: &str) -> botsu_core::Result<()> {
        ChannelId::new(snowflake(channel_id)?)
            .delete_message(&self.http, MessageId::new(snowflake(message_id)?))
            .await
            .map_err(err)
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
}

// --- Voice (songbird) ---------------------------------------------------------------------------

/// Discord voice through songbird (which handles DAVE end-to-end encryption). Who is in which
/// voice channel is tracked from GUILD_CREATE snapshots and VOICE_STATE_UPDATE events, so the
/// memory-hungry serenity cache isn't needed.
pub struct DiscordVoice {
    songbird: Arc<Songbird>,
    /// (guild, user) → voice channel.
    states: Mutex<HashMap<(u64, u64), u64>>,
}

impl DiscordVoice {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            songbird: Songbird::serenity(),
            states: Mutex::new(HashMap::new()),
        })
    }

    fn states(&self) -> std::sync::MutexGuard<'_, HashMap<(u64, u64), u64>> {
        self.states.lock().unwrap_or_else(|p| p.into_inner())
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
fn ffmpeg_input(source: &str) -> botsu_core::Result<Input> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);
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
            "-i", source, "-f", "f32le", "-ar", "48000", "-ac", "2", "pipe:1",
        ])
        .stdin(Stdio::null())
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
        let key = (guild_id.parse().ok()?, user_id.parse().ok()?);
        self.states().get(&key).map(u64::to_string)
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

    async fn play(&self, guild_id: &str, source: &str) -> botsu_core::Result<Option<String>> {
        let call = self
            .songbird
            .get(GuildId::new(snowflake(guild_id)?))
            .ok_or_else(|| Error::Usage("I'm not in a voice channel; join first.".into()))?;
        let input = ffmpeg_input(source)?;
        let (track, channel) = {
            let mut call = call.lock().await;
            (call.play_only_input(input), call.current_channel())
        };

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
        let _ = self
            .songbird
            .remove(GuildId::new(snowflake(guild_id)?))
            .await;
        Ok(())
    }
}

/// Inbound side: receives gateway events and forwards them to the core.
struct Handler {
    bot: Arc<Bot>,
    voice: Arc<DiscordVoice>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn guild_create(&self, _ctx: Context, guild: Guild, _is_new: Option<bool>) {
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

    async fn ready(&self, _ctx: Context, ready: Ready) {
        tracing::info!(
            user = %ready.user.name,
            guilds = ready.guilds.len(),
            "connected to Discord"
        );
        tracing::info!(
            "Discord invite link: https://discord.com/oauth2/authorize?client_id={}&scope=bot&permissions={}",
            ready.application.id,
            botsu_core::INVITE_PERMISSIONS
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
