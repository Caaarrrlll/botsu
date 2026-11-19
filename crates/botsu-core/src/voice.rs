//! Voice: the platform-neutral interface (Discord: songbird; Fluxer: LiveKit), the idle timer, and
//! the `audio` test command. Phase 0 uses it to check that both platforms can play audio; phase 6
//! (music) builds the queue on top of the same interface.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::adapter::Platform;
use crate::command::{Command, CommandContext};
use crate::error::{Error, Result};
use crate::permissions::Level;

/// What the core needs from a platform's voice support. All IDs are strings, as elsewhere.
#[async_trait]
pub trait VoiceAdapter: Send + Sync {
    fn platform(&self) -> Platform;

    /// The voice channel a user is in, if the bot can see it.
    async fn user_channel(&self, guild_id: &str, user_id: &str) -> Option<String>;

    /// Everyone the bot can see in voice in a server, as `(user, channel)`.
    async fn voice_members(&self, guild_id: &str) -> Vec<(String, String)>;

    async fn join(&self, guild_id: &str, channel_id: &str) -> Result<()>;

    /// Plays `source` (a file path or URL; anything ffmpeg reads), replacing what's playing.
    /// May return a diagnostic note for the user (for example what the audio engine reports).
    async fn play(&self, guild_id: &str, source: &str) -> Result<Option<String>>;

    async fn stop(&self, guild_id: &str) -> Result<()>;

    async fn leave(&self, guild_id: &str) -> Result<()>;

    /// Whether the bot is currently in a voice channel in this server.
    async fn is_connected(&self, guild_id: &str) -> bool;

    /// Whether audio is playing right now in this server (for the idle timer).
    async fn is_playing(&self, guild_id: &str) -> bool;
}

/// Leave a voice channel after this long without playing anything.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const IDLE_CHECK_EVERY: Duration = Duration::from_secs(30);

/// Watches one server's voice connection and leaves after `IDLE_TIMEOUT` of silence. Exits when
/// the bot is no longer connected there. At most one watcher per (platform, server).
fn spawn_idle_watch(
    voice: Arc<dyn VoiceAdapter>,
    guild: String,
    watching: Arc<Mutex<HashSet<(Platform, String)>>>,
) {
    let key = (voice.platform(), guild.clone());
    if !watching
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.clone())
    {
        return; // already watched
    }
    tokio::spawn(async move {
        let mut silent_for = Duration::ZERO;
        loop {
            tokio::time::sleep(IDLE_CHECK_EVERY).await;
            if !voice.is_connected(&guild).await {
                break;
            }
            if voice.is_playing(&guild).await {
                silent_for = Duration::ZERO;
                continue;
            }
            silent_for += IDLE_CHECK_EVERY;
            if silent_for >= IDLE_TIMEOUT {
                tracing::info!(platform = %voice.platform(), guild, "leaving voice: idle for 10 minutes");
                if let Err(e) = voice.leave(&guild).await {
                    tracing::warn!(error = %e, "leaving idle voice channel failed");
                }
                break;
            }
        }
        watching
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&key);
    });
}

/// Creates (once) a 10-second 440 Hz test tone with ffmpeg and returns its path.
pub async fn test_tone() -> Result<PathBuf> {
    let path = std::env::temp_dir().join("botsu-test-tone.wav");
    if path.exists() {
        return Ok(path);
    }
    let status = tokio::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:duration=10")
        .arg(&path)
        .status()
        .await
        .map_err(|e| Error::internal("running ffmpeg (is it installed?)", e))?;
    if !status.success() {
        return Err(Error::Internal(format!(
            "ffmpeg couldn't create the test tone ({status})"
        )));
    }
    Ok(path)
}

/// `audio join|play|stop|leave|where`: manual voice test, bot owners only. Leaves a voice
/// channel by itself after 10 minutes without sound.
pub struct AudioCommand {
    adapters: Vec<Arc<dyn VoiceAdapter>>,
    watching: Arc<Mutex<HashSet<(Platform, String)>>>,
}

impl AudioCommand {
    pub fn new(adapters: Vec<Arc<dyn VoiceAdapter>>) -> Self {
        Self {
            adapters,
            watching: Arc::default(),
        }
    }

    fn usage(p: &str) -> String {
        format!(
            "Audio test (bot owners):\n\
             `{p}audio join [channel]`: join your voice channel, or the given one\n\
             `{p}audio play [url]`: play a 10-second test tone, or a direct audio link\n\
             `{p}audio stop` / `{p}audio leave`\n\
             `{p}audio where`: who I can see in voice here\n\
             I leave by myself after 10 minutes without sound."
        )
    }
}

#[async_trait]
impl Command for AudioCommand {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn help(&self) -> &'static str {
        "Audio test: join, play, stop, leave, where (bot owners)."
    }

    fn level(&self) -> Level {
        Level::BotOwner
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        ctx.require_owner()?;
        let platform = ctx.message.platform;
        let voice = self
            .adapters
            .iter()
            .find(|a| a.platform() == platform)
            .ok_or(Error::Unsupported {
                platform,
                op: "audio",
            })?;
        let Some(guild) = ctx.message.guild_id.clone() else {
            return Err(Error::Usage("Voice only works in a server.".into()));
        };
        let (sub, arg) = ctx
            .args
            .split_once(char::is_whitespace)
            .map(|(s, a)| (s.to_lowercase(), a.trim().to_owned()))
            .unwrap_or_else(|| (ctx.args.to_lowercase(), String::new()));

        match sub.as_str() {
            "join" => {
                let target = if arg.is_empty() {
                    voice.user_channel(&guild, &ctx.message.author.id).await
                } else {
                    Some(
                        arg.trim_start_matches("<#")
                            .trim_end_matches('>')
                            .to_owned(),
                    )
                };
                let Some(target) = target else {
                    return Err(Error::Usage(format!(
                        "I can't see you in a voice channel. Join one (or leave and rejoin it), \
                         or use `{}audio join <channel id>`.",
                        ctx.prefix
                    )));
                };
                ctx.reply(&format!("Joining <#{target}>...")).await?;
                voice.join(&guild, &target).await?;
                spawn_idle_watch(voice.clone(), guild.clone(), self.watching.clone());
                ctx.reply(&format!(
                    "Joined. Try `{}audio play`. I leave after 10 minutes without sound.",
                    ctx.prefix
                ))
                .await?;
            }
            "play" => {
                let source = if arg.is_empty() {
                    test_tone().await?.to_string_lossy().into_owned()
                } else {
                    arg.trim_start_matches('<').trim_end_matches('>').to_owned()
                };
                let note = voice.play(&guild, &source).await?;
                let shown = if Path::new(&source)
                    .file_name()
                    .is_some_and(|n| n == "botsu-test-tone.wav")
                {
                    "the test tone (10 s, 440 Hz)".to_owned()
                } else {
                    format!("<{source}>")
                };
                let mut reply = format!("Playing {shown}.");
                if let Some(note) = note {
                    reply.push(' ');
                    reply.push_str(&note);
                }
                ctx.reply(&reply).await?;
            }
            "stop" => {
                voice.stop(&guild).await?;
                ctx.reply("Stopped.").await?;
            }
            "leave" => {
                voice.leave(&guild).await?;
                ctx.reply("Left.").await?;
            }
            "where" => {
                let members = voice.voice_members(&guild).await;
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
            _ => return Err(Error::Usage(Self::usage(&ctx.prefix))),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Connected until `leave`; plays while `playing` is set.
    #[derive(Default)]
    struct FakeVoice {
        playing: AtomicBool,
        left: AtomicBool,
    }

    #[async_trait]
    impl VoiceAdapter for FakeVoice {
        fn platform(&self) -> Platform {
            Platform::Discord
        }
        async fn user_channel(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        async fn voice_members(&self, _: &str) -> Vec<(String, String)> {
            Vec::new()
        }
        async fn join(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn play(&self, _: &str, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn stop(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn leave(&self, _: &str) -> Result<()> {
            self.left.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn is_connected(&self, _: &str) -> bool {
            !self.left.load(Ordering::SeqCst)
        }
        async fn is_playing(&self, _: &str) -> bool {
            self.playing.load(Ordering::SeqCst)
        }
    }

    async fn advance(d: Duration) {
        // Step in check-sized increments so the watcher task runs at each tick.
        let mut done = Duration::ZERO;
        while done < d {
            tokio::time::advance(IDLE_CHECK_EVERY).await;
            tokio::task::yield_now().await;
            done += IDLE_CHECK_EVERY;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn leaves_after_ten_silent_minutes() {
        let voice = Arc::new(FakeVoice::default());
        let watching = Arc::default();
        spawn_idle_watch(voice.clone(), "g".into(), Arc::clone(&watching));
        advance(IDLE_TIMEOUT - IDLE_CHECK_EVERY).await;
        assert!(!voice.left.load(Ordering::SeqCst), "left too early");
        advance(IDLE_CHECK_EVERY * 2).await;
        assert!(voice.left.load(Ordering::SeqCst), "should have left");
        assert!(
            watching.lock().unwrap().is_empty(),
            "watcher should be gone"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn playing_keeps_it_in_and_one_watcher_per_server() {
        let voice = Arc::new(FakeVoice::default());
        voice.playing.store(true, Ordering::SeqCst);
        let watching: Arc<Mutex<HashSet<(Platform, String)>>> = Arc::default();
        spawn_idle_watch(voice.clone(), "g".into(), Arc::clone(&watching));
        spawn_idle_watch(voice.clone(), "g".into(), Arc::clone(&watching));
        assert_eq!(watching.lock().unwrap().len(), 1);
        advance(IDLE_TIMEOUT * 2).await;
        assert!(!voice.left.load(Ordering::SeqCst), "left while playing");
        // Silence starts the countdown from zero.
        voice.playing.store(false, Ordering::SeqCst);
        advance(IDLE_TIMEOUT + IDLE_CHECK_EVERY).await;
        assert!(voice.left.load(Ordering::SeqCst));
    }
}
