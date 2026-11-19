//! Music (phase 6): a queue per server, played through the platform's [`VoiceAdapter`].
//!
//! `music play <search or link>` looks the track up with yt-dlp (title, length, page URL) and
//! queues it. A player task per server takes tracks off the queue, resolves the page URL to a
//! stream URL just before playing (they expire), plays it, and waits until it ends or is skipped.
//! The idle timer in `voice.rs` leaves the channel after 10 minutes without sound.
//!
//! Anyone in the bot's voice channel can control the queue; mods can from anywhere. The voice
//! test tools (`test`, `where`) and `update` (yt-dlp) are for bot owners.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Notify;

use crate::adapter::{ChatAdapter, Platform};
use crate::command::{Command, CommandContext, Group};
use crate::error::{Error, Result};
use crate::permissions::Level;
use crate::voice::{spawn_idle_watch, test_tone, VoiceAdapter};
use crate::ytdlp::{is_url, YtDlp};

/// Most tracks taken from one playlist link, and most queued per server.
const PLAYLIST_MAX: usize = 100;
const QUEUE_MAX: usize = 500;
/// How often the player checks whether the track is still playing.
const POLL: Duration = Duration::from_secs(1);
/// A track only counts as finished after this long, so a slow start isn't taken as the end.
const START_GRACE: Duration = Duration::from_secs(5);
const MAX_MESSAGE: usize = 1900;

#[derive(Debug, Clone)]
pub struct Track {
    pub title: String,
    pub url: String,
    pub duration: Option<u64>,
    /// Display name of whoever queued it.
    pub requester: String,
}

/// `213` → `3:33`, `3725` → `1:02:05`.
pub fn format_length(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn describe(t: &Track) -> String {
    match t.duration {
        Some(d) => format!("**{}** ({})", t.title, format_length(d)),
        None => format!("**{}**", t.title),
    }
}

type Key = (Platform, String);

/// One server's music state.
struct Guild {
    queue: VecDeque<Track>,
    current: Option<Track>,
    /// Where "now playing" goes: the channel of the last `play`.
    text_channel: String,
    chat: Arc<dyn ChatAdapter>,
    /// The player task is running.
    running: bool,
    /// Wakes the player to skip the current track. New for each player task, so a skip sent
    /// while nothing plays can't skip the next track later.
    skip: Arc<Notify>,
}

pub struct Music {
    voices: Vec<Arc<dyn VoiceAdapter>>,
    ytdlp: Arc<YtDlp>,
    guilds: Mutex<HashMap<Key, Guild>>,
    watching: Arc<Mutex<HashSet<(Platform, String)>>>,
}

impl Music {
    pub fn new(voices: Vec<Arc<dyn VoiceAdapter>>, ytdlp: Arc<YtDlp>) -> Arc<Self> {
        Arc::new(Self {
            voices,
            ytdlp,
            guilds: Mutex::default(),
            watching: Arc::default(),
        })
    }

    fn guilds(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Guild>> {
        self.guilds.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn voice(&self, platform: Platform) -> Result<Arc<dyn VoiceAdapter>> {
        self.voices
            .iter()
            .find(|v| v.platform() == platform)
            .cloned()
            .ok_or(Error::Unsupported {
                platform,
                op: "music",
            })
    }

    /// Adds tracks; starts the player if it isn't running. Returns the queue position of the
    /// first one (0 = it plays right away).
    fn enqueue(
        self: &Arc<Self>,
        voice: Arc<dyn VoiceAdapter>,
        guild: &str,
        chat: Arc<dyn ChatAdapter>,
        text_channel: &str,
        tracks: Vec<Track>,
    ) -> Result<usize> {
        let key = (voice.platform(), guild.to_owned());
        let mut guilds = self.guilds();
        let g = guilds.entry(key.clone()).or_insert_with(|| Guild {
            queue: VecDeque::new(),
            current: None,
            text_channel: text_channel.to_owned(),
            chat: chat.clone(),
            running: false,
            skip: Arc::new(Notify::new()),
        });
        if g.queue.len() + tracks.len() > QUEUE_MAX {
            return Err(Error::Usage(format!(
                "The queue is limited to {QUEUE_MAX} tracks."
            )));
        }
        g.text_channel = text_channel.to_owned();
        g.chat = chat;
        let busy = g.running && (g.current.is_some() || !g.queue.is_empty());
        let position = if busy { g.queue.len() + 1 } else { 0 };
        g.queue.extend(tracks);
        if !g.running {
            g.running = true;
            g.skip = Arc::new(Notify::new());
            let music = self.clone();
            tokio::spawn(async move { music.player(voice, key).await });
        }
        Ok(position)
    }

    /// Plays the queue until it's empty or the bot is disconnected.
    async fn player(self: Arc<Self>, voice: Arc<dyn VoiceAdapter>, key: Key) {
        let guild = key.1.clone();
        loop {
            let next = {
                let mut guilds = self.guilds();
                let Some(g) = guilds.get_mut(&key) else {
                    return;
                };
                match g.queue.pop_front() {
                    Some(t) => {
                        g.current = Some(t.clone());
                        Some((t, g.chat.clone(), g.text_channel.clone(), g.skip.clone()))
                    }
                    None => {
                        g.current = None;
                        g.running = false;
                        None
                    }
                }
            };
            let Some((track, chat, channel, skip)) = next else {
                return;
            };
            let say = |text: String| {
                let (chat, channel) = (chat.clone(), channel.clone());
                async move {
                    if let Err(e) = chat.send_message(&channel, &text).await {
                        tracing::warn!(error = %e, "couldn't post a music message");
                    }
                }
            };

            if !voice.is_connected(&guild).await {
                self.clear(&key);
                return;
            }
            let stream = match self.ytdlp.stream_url(&track.url).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::info!(error = %e, url = %track.url, "couldn't resolve a track");
                    say(format!("Skipping {}: {e}", describe(&track))).await;
                    continue;
                }
            };
            if let Err(e) = voice.play(&guild, &stream).await {
                tracing::info!(error = %e, url = %track.url, "couldn't play a track");
                say(format!("Skipping {}: {e}", describe(&track))).await;
                continue;
            }
            say(format!(
                "Now playing {} · requested by {}\n<{}>",
                describe(&track),
                track.requester,
                track.url
            ))
            .await;

            let started = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = skip.notified() => {
                        let _ = voice.stop(&guild).await;
                        break;
                    }
                    _ = tokio::time::sleep(POLL) => {}
                }
                if !voice.is_connected(&guild).await {
                    self.clear(&key);
                    return;
                }
                if started.elapsed() >= START_GRACE && !voice.is_playing(&guild).await {
                    break;
                }
            }
        }
    }

    /// Empties the queue and skips the current track. True if anything was queued or playing.
    fn clear(&self, key: &Key) -> bool {
        let mut guilds = self.guilds();
        let Some(g) = guilds.get_mut(key) else {
            return false;
        };
        let had = g.current.is_some() || !g.queue.is_empty();
        g.queue.clear();
        if g.running {
            g.skip.notify_one();
        }
        had
    }

    fn skip(&self, key: &Key) -> Option<Track> {
        let guilds = self.guilds();
        let g = guilds.get(key)?;
        let current = g.current.clone()?;
        g.skip.notify_one();
        Some(current)
    }

    /// `(current, queue)`.
    fn snapshot(&self, key: &Key) -> (Option<Track>, Vec<Track>) {
        let guilds = self.guilds();
        match guilds.get(key) {
            Some(g) => (g.current.clone(), g.queue.iter().cloned().collect()),
            None => (None, Vec::new()),
        }
    }

    fn with_queue<T>(&self, key: &Key, f: impl FnOnce(&mut VecDeque<Track>) -> T) -> Option<T> {
        let mut guilds = self.guilds();
        guilds.get_mut(key).map(|g| f(&mut g.queue))
    }
}

// --- commands -------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    Play,
    Skip,
    Stop,
    Queue,
    Now,
    Remove,
    Clear,
    Shuffle,
    Join,
    Leave,
    Test,
    Where,
    Update,
}

impl Sub {
    const ALL: [Sub; 13] = [
        Sub::Play,
        Sub::Skip,
        Sub::Stop,
        Sub::Queue,
        Sub::Now,
        Sub::Remove,
        Sub::Clear,
        Sub::Shuffle,
        Sub::Join,
        Sub::Leave,
        Sub::Test,
        Sub::Where,
        Sub::Update,
    ];
}

/// `music <command>`.
pub fn group(music: &Arc<Music>) -> Group {
    let mut group = Group::new(
        "music",
        "Music",
        "Music: play, skip, stop, queue, now, remove, clear, shuffle, join, leave.",
    );
    for sub in Sub::ALL {
        group = group.with(MusicCommand {
            sub,
            music: music.clone(),
        });
    }
    group
}

struct MusicCommand {
    sub: Sub,
    music: Arc<Music>,
}

struct Here {
    voice: Arc<dyn VoiceAdapter>,
    guild: String,
    key: Key,
}

impl MusicCommand {
    fn here(&self, ctx: &CommandContext) -> Result<Here> {
        let platform = ctx.message.platform;
        let voice = self.music.voice(platform)?;
        let guild = ctx
            .message
            .guild_id
            .clone()
            .ok_or_else(|| Error::Usage("Music only works in a server.".into()))?;
        Ok(Here {
            voice,
            key: (platform, guild.clone()),
            guild,
        })
    }

    /// Queue controls are for people listening with the bot (mods: from anywhere).
    async fn require_listener(&self, ctx: &CommandContext, here: &Here) -> Result<()> {
        if ctx.level >= Level::Mod || !here.voice.is_connected(&here.guild).await {
            return Ok(());
        }
        let Ok(bot) = ctx.chat.bot_user_id().await else {
            return Ok(());
        };
        let Some(bot_channel) = here.voice.user_channel(&here.guild, &bot).await else {
            return Ok(());
        };
        let user = here
            .voice
            .user_channel(&here.guild, &ctx.message.author.id)
            .await;
        if user.as_deref() == Some(bot_channel.as_str()) {
            Ok(())
        } else {
            Err(Error::Usage(format!(
                "Join <#{bot_channel}> to control the music."
            )))
        }
    }

    /// Joins the caller's voice channel unless already connected.
    async fn connect(&self, ctx: &CommandContext, here: &Here) -> Result<()> {
        if here.voice.is_connected(&here.guild).await {
            return self.require_listener(ctx, here).await;
        }
        let Some(channel) = here
            .voice
            .user_channel(&here.guild, &ctx.message.author.id)
            .await
        else {
            return Err(Error::Usage(
                "Join a voice channel first (if you're already in one, leave and rejoin it so \
                 I can see you)."
                    .into(),
            ));
        };
        here.voice.join(&here.guild, &channel).await?;
        spawn_idle_watch(
            here.voice.clone(),
            here.guild.clone(),
            self.music.watching.clone(),
        );
        Ok(())
    }

    async fn play(&self, ctx: &CommandContext, here: &Here) -> Result<()> {
        let query = ctx
            .args
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        if query.is_empty() {
            return Err(Error::Usage(format!(
                "Usage: `{}play <search words or link>`",
                ctx.prefix
            )));
        }
        self.connect(ctx, here).await?;
        let found = self
            .music
            .ytdlp
            .lookup(query, PLAYLIST_MAX)
            .await
            .map_err(|e| {
                tracing::info!(error = %e, query, "music lookup failed");
                Error::Usage(format!("I couldn't find that: {e}"))
            })?;
        if found.is_empty() {
            return Err(Error::Usage(format!("Nothing found for `{query}`.")));
        }
        let count = found.len();
        let tracks: Vec<Track> = found
            .into_iter()
            .map(|f| Track {
                title: f.title,
                url: f.url,
                duration: f.duration,
                requester: ctx.message.author.name.clone(),
            })
            .collect();
        let first = describe(&tracks[0]);
        let position = self.music.enqueue(
            here.voice.clone(),
            &here.guild,
            ctx.chat.clone(),
            &ctx.message.channel_id,
            tracks,
        )?;
        let reply = match (count, position) {
            (1, 0) => return Ok(()), // the player announces it
            (1, n) => format!("Queued {first} at position {n}."),
            (n, _) if is_url(query) => format!("Queued {n} tracks from the playlist."),
            (n, _) => format!("Queued {n} tracks."),
        };
        ctx.reply(&reply).await?;
        Ok(())
    }

    async fn queue(&self, ctx: &CommandContext, here: &Here) -> Result<()> {
        let (current, queue) = self.music.snapshot(&here.key);
        let Some(current) = current else {
            ctx.reply("Nothing is playing.").await?;
            return Ok(());
        };
        let mut out = format!("Now playing {}", describe(&current));
        if !queue.is_empty() {
            let total: u64 = queue.iter().filter_map(|t| t.duration).sum();
            out.push_str(&format!(
                "\n**Up next** ({} tracks, {}):",
                queue.len(),
                format_length(total)
            ));
            for (i, t) in queue.iter().enumerate() {
                let line = format!("\n{}. {} · {}", i + 1, describe(t), t.requester);
                if out.len() + line.len() > MAX_MESSAGE {
                    out.push_str(&format!("\n… and {} more", queue.len() - i));
                    break;
                }
                out.push_str(&line);
            }
        }
        ctx.reply(&out).await?;
        Ok(())
    }
}

#[async_trait]
impl Command for MusicCommand {
    fn name(&self) -> &'static str {
        match self.sub {
            Sub::Play => "play",
            Sub::Skip => "skip",
            Sub::Stop => "stop",
            Sub::Queue => "queue",
            Sub::Now => "now",
            Sub::Remove => "remove",
            Sub::Clear => "clear",
            Sub::Shuffle => "shuffle",
            Sub::Join => "join",
            Sub::Leave => "leave",
            Sub::Test => "test",
            Sub::Where => "where",
            Sub::Update => "update",
        }
    }

    fn help(&self) -> &'static str {
        match self.sub {
            Sub::Play => {
                "Play or queue a song: search words, or a YouTube (or other) link or playlist."
            }
            Sub::Skip => "Skip the current song.",
            Sub::Stop => "Stop and empty the queue.",
            Sub::Queue => "Show the queue.",
            Sub::Now => "Show what's playing.",
            Sub::Remove => "Remove a song from the queue by its number.",
            Sub::Clear => "Empty the queue (the current song keeps playing).",
            Sub::Shuffle => "Shuffle the queue.",
            Sub::Join => "Join your voice channel.",
            Sub::Leave => "Stop and leave the voice channel.",
            Sub::Test => "Play a test tone or a direct audio link (bot owners).",
            Sub::Where => "Who I can see in voice here (bot owners).",
            Sub::Update => "Update yt-dlp now (bot owners).",
        }
    }

    fn level(&self) -> Level {
        match self.sub {
            Sub::Test | Sub::Where | Sub::Update => Level::BotOwner,
            _ => Level::Everyone,
        }
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        if self.sub == Sub::Update {
            let before = self.music.ytdlp.version().await;
            let out = self.music.ytdlp.update().await?;
            let after = self.music.ytdlp.version().await;
            ctx.reply(&format!(
                "yt-dlp: {out}\nVersion: {} → {}",
                before.as_deref().unwrap_or("?"),
                after.as_deref().unwrap_or("?")
            ))
            .await?;
            return Ok(());
        }
        let here = self.here(&ctx)?;
        match self.sub {
            Sub::Play => self.play(&ctx, &here).await?,
            Sub::Queue => self.queue(&ctx, &here).await?,
            Sub::Now => {
                let text = match self.music.snapshot(&here.key).0 {
                    Some(t) => format!(
                        "Now playing {} · requested by {}\n<{}>",
                        describe(&t),
                        t.requester,
                        t.url
                    ),
                    None => "Nothing is playing.".into(),
                };
                ctx.reply(&text).await?;
            }
            Sub::Skip => {
                self.require_listener(&ctx, &here).await?;
                let text = match self.music.skip(&here.key) {
                    Some(t) => format!("Skipped {}.", describe(&t)),
                    None => "Nothing is playing.".into(),
                };
                ctx.reply(&text).await?;
            }
            Sub::Stop => {
                self.require_listener(&ctx, &here).await?;
                self.music.clear(&here.key);
                here.voice.stop(&here.guild).await?;
                ctx.reply("Stopped and emptied the queue.").await?;
            }
            Sub::Clear => {
                self.require_listener(&ctx, &here).await?;
                let n = self
                    .music
                    .with_queue(&here.key, |q| std::mem::take(q).len())
                    .unwrap_or(0);
                ctx.reply(&format!("Removed {n} track(s) from the queue."))
                    .await?;
            }
            Sub::Remove => {
                self.require_listener(&ctx, &here).await?;
                let n: usize = ctx.args.trim().parse().map_err(|_| {
                    Error::Usage(format!(
                        "Usage: `{}remove <number>` (see `{}queue`)",
                        ctx.prefix, ctx.prefix
                    ))
                })?;
                let removed = self
                    .music
                    .with_queue(&here.key, |q| n.checked_sub(1).and_then(|i| q.remove(i)))
                    .flatten()
                    .ok_or_else(|| Error::Usage(format!("There's no number {n} in the queue.")))?;
                ctx.reply(&format!("Removed {}.", describe(&removed)))
                    .await?;
            }
            Sub::Shuffle => {
                self.require_listener(&ctx, &here).await?;
                let n = self
                    .music
                    .with_queue(&here.key, |q| {
                        shuffle(q.make_contiguous());
                        q.len()
                    })
                    .unwrap_or(0);
                ctx.reply(&format!("Shuffled {n} track(s).")).await?;
            }
            Sub::Join => {
                self.connect(&ctx, &here).await?;
                ctx.reply("Here. I leave after 10 minutes without sound.")
                    .await?;
            }
            Sub::Leave => {
                self.require_listener(&ctx, &here).await?;
                self.music.clear(&here.key);
                here.voice.leave(&here.guild).await?;
                ctx.reply("Left.").await?;
            }
            Sub::Test => {
                self.connect(&ctx, &here).await?;
                let arg = ctx
                    .args
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>');
                let source = if arg.is_empty() {
                    test_tone().await?.to_string_lossy().into_owned()
                } else {
                    arg.to_owned()
                };
                // Bypasses the queue: anything queued is cleared.
                self.music.clear(&here.key);
                let note = here.voice.play(&here.guild, &source).await?;
                let mut reply = if arg.is_empty() {
                    "Playing the test tone (10 s, 440 Hz).".to_owned()
                } else {
                    format!("Playing <{arg}>.")
                };
                if let Some(note) = note {
                    reply.push(' ');
                    reply.push_str(&note);
                }
                ctx.reply(&reply).await?;
            }
            Sub::Where => {
                let members = here.voice.voice_members(&here.guild).await;
                let text = if members.is_empty() {
                    "I can't see anyone in a voice channel here.".to_owned()
                } else {
                    members
                        .iter()
                        .map(|(u, c)| format!("• `{u}` in <#{c}>"))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                ctx.reply(&text).await?;
            }
            Sub::Update => unreachable!("handled above"),
        }
        Ok(())
    }
}

/// Fisher–Yates with a small xorshift seeded from the clock (no `rand` dependency needed).
fn shuffle<T>(items: &mut [T]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert_eq!(format_length(0), "0:00");
        assert_eq!(format_length(213), "3:33");
        assert_eq!(format_length(3725), "1:02:05");
    }

    #[test]
    fn shuffle_keeps_items() {
        let mut v: Vec<u32> = (0..50).collect();
        shuffle(&mut v);
        let mut sorted = v.clone();
        sorted.sort();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
    }
}
