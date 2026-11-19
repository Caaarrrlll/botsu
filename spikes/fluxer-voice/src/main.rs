//! Phase 0 spike: can a bot join a Fluxer voice channel and play audio through LiveKit?
//! Also checks that Fluxer tells bots who is in which voice channel (voice states).
//!
//! Run from this folder: `cargo run`. Reads FLUXER_TOKEN and FLUXER_URL from ../../.env.
//! Commands (prefix !): join [channel id], play [file or URL], stop, leave, where.

use std::collections::HashMap;
use std::path::Path;

use fluxer::prelude::*;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

const TONE: &str = "../test-tone.mp3";

struct Handler {
    /// (guild, user) → voice channel, from the guild snapshot and voice state updates.
    voice: Mutex<HashMap<(String, String), String>>,
    conn: Mutex<Option<FluxerVoiceConnection>>,
    playback: Mutex<Option<AbortHandle>>,
}

impl Handler {
    async fn snapshot(&self, source: &str, guild: &str, states: Vec<serde_json::Value>) {
        let mut voice = self.voice.lock().await;
        for vs in &states {
            if let (Some(user), Some(channel)) = (vs["user_id"].as_str(), vs["channel_id"].as_str()) {
                voice.insert((guild.to_owned(), user.to_owned()), channel.to_owned());
            }
        }
        println!("{source} for guild {guild}: {} voice state(s)", states.len());
    }

    async fn say(&self, ctx: &Context, channel: &str, text: &str) {
        println!("> {text}");
        let _ = ctx.http.send_message(channel, text).await;
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn on_ready(&self, _ctx: Context, ready: Ready) {
        println!("Logged in as {}", ready.user.username);
    }

    // Snapshots of who is already in voice. fluxer-rust's Guild (GUILD_CREATE) drops the field, so
    // these come from GUILD_SYNC and PASSIVE_UPDATES; the log says which ones actually arrive.
    async fn on_guild_sync(&self, _ctx: Context, sync: GuildSync) {
        self.snapshot("GUILD_SYNC", &sync.id, sync.voice_states.unwrap_or_default())
            .await;
    }

    async fn on_passive_updates(&self, _ctx: Context, p: PassiveUpdates) {
        self.snapshot("PASSIVE_UPDATES", &p.guild_id, p.voice_states.unwrap_or_default())
            .await;
    }

    async fn on_voice_state_update(&self, _ctx: Context, v: VoiceStateUpdate) {
        let Some(guild) = v.guild_id else { return };
        println!("Voice state: user {} → {:?}", v.user_id, v.channel_id);
        let mut voice = self.voice.lock().await;
        match v.channel_id {
            Some(ch) => voice.insert((guild, v.user_id), ch),
            None => voice.remove(&(guild, v.user_id)),
        };
    }

    async fn on_message(&self, ctx: Context, msg: Message) {
        if msg.author.bot.unwrap_or(false) {
            return;
        }
        let (Some(content), Some(channel), Some(guild)) =
            (msg.content.as_deref(), msg.channel_id.as_deref(), msg.guild_id.as_deref())
        else {
            return;
        };
        let Some(rest) = content.strip_prefix('!') else { return };
        let (cmd, arg) = rest.split_once(' ').map_or((rest, ""), |(c, a)| (c, a.trim()));

        match cmd {
            "where" => {
                let voice = self.voice.lock().await;
                let mine: Vec<String> = voice
                    .iter()
                    .filter(|((g, _), _)| g == guild)
                    .map(|((_, u), c)| format!("user {u} in {c}"))
                    .collect();
                let text = if mine.is_empty() {
                    "I can't see anyone in a voice channel here.".to_owned()
                } else {
                    mine.join("\n")
                };
                self.say(&ctx, channel, &text).await;
            }
            "join" => {
                let target = if arg.is_empty() {
                    self.voice
                        .lock()
                        .await
                        .get(&(guild.to_owned(), msg.author.id.clone()))
                        .cloned()
                } else {
                    Some(arg.to_owned())
                };
                let Some(target) = target else {
                    self.say(&ctx, channel, "I can't see you in a voice channel. Join one first, or use `!join <channel id>`.").await;
                    return;
                };
                self.say(&ctx, channel, &format!("Joining voice channel {target}...")).await;
                match ctx.join_voice(guild, &target).await {
                    Ok(conn) => {
                        *self.conn.lock().await = Some(conn);
                        self.say(&ctx, channel, "Joined. Try `!play`.").await;
                    }
                    Err(e) => self.say(&ctx, channel, &format!("Join failed: {e}")).await,
                }
            }
            "play" => {
                let source = if arg.is_empty() { TONE } else { arg };
                let conn = self.conn.lock().await;
                let Some(conn) = conn.as_ref() else {
                    self.say(&ctx, channel, "Not in a voice channel; `!join` first.").await;
                    return;
                };
                if let Some(h) = self.playback.lock().await.take() {
                    h.abort();
                }
                match conn.play_music(source, ctx.http.clone(), channel.to_owned()).await {
                    Ok(handle) => {
                        *self.playback.lock().await = Some(handle);
                        self.say(&ctx, channel, &format!("Playing `{source}`.")).await;
                    }
                    Err(e) => self.say(&ctx, channel, &format!("Play failed: {e}")).await,
                }
            }
            "stop" => {
                if let Some(h) = self.playback.lock().await.take() {
                    h.abort();
                }
                self.say(&ctx, channel, "Stopped.").await;
            }
            "leave" => {
                if let Some(h) = self.playback.lock().await.take() {
                    h.abort();
                }
                *self.conn.lock().await = None;
                let _ = ctx.leave_voice(guild).await;
                self.say(&ctx, channel, "Left.").await;
            }
            _ => {}
        }
    }
}

/// KEY=value lines from ../../.env (the repo's local settings), without overriding real env vars.
fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string("../../.env") else { return };
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() && std::env::var_os(k).is_none() {
                // Single-threaded at this point (before the runtime starts).
                std::env::set_var(k, v);
            }
        }
    }
}

/// A 10-second 440 Hz test tone, made with ffmpeg the first time.
fn ensure_tone() {
    if Path::new(TONE).exists() {
        return;
    }
    let ok = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=10", TONE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("Couldn't create {TONE}: is ffmpeg installed and on PATH? (winget install Gyan.FFmpeg)");
        std::process::exit(1);
    }
}

fn main() {
    load_dotenv();
    ensure_tone();
    let token = std::env::var("FLUXER_TOKEN").expect("FLUXER_TOKEN not set (../../.env)");
    let api = std::env::var("FLUXER_API_URL").unwrap_or_else(|_| {
        let site = std::env::var("FLUXER_URL").unwrap_or_else(|_| "https://fluxer.com".into());
        // Same layout botsu discovers: <site>/api/v1. Strip any path pasted from the address bar.
        let scheme_end = site.find("://").map(|i| i + 3).unwrap_or(0);
        let origin = match site[scheme_end..].find('/') {
            Some(i) => &site[..scheme_end + i],
            None => &site,
        };
        format!("{origin}/api/v1")
    });
    println!("Fluxer API: {api}");

    tokio::runtime::Runtime::new().unwrap().block_on(async move {
        let mut client = Client::builder(&token)
            .api_url(&api)
            .event_handler(Handler {
                voice: Mutex::new(HashMap::new()),
                conn: Mutex::new(None),
                playback: Mutex::new(None),
            })
            .build();
        if let Err(e) = client.start().await {
            eprintln!("Error: {e}");
        }
    });
}
