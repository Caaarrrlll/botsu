//! Voice: the platform-neutral interface (Discord: songbird; Fluxer: LiveKit), the idle timer and
//! the test tone. The queue and the `music` commands are in `music.rs`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::adapter::Platform;
use crate::error::{Error, Result};

/// What to play. Both platforms decode it with ffmpeg.
pub enum AudioInput {
    /// A file path or URL that ffmpeg opens itself.
    Path(String),
    /// Audio on ffmpeg's stdin, for example from `yt-dlp -o -`.
    Pipe(std::process::Stdio),
}

/// What the core needs from a platform's voice support. All IDs are strings, as elsewhere.
#[async_trait]
pub trait VoiceAdapter: Send + Sync {
    fn platform(&self) -> Platform;

    /// The voice channel a user is in, if the bot can see it.
    async fn user_channel(&self, guild_id: &str, user_id: &str) -> Option<String>;

    /// Everyone the bot can see in voice in a server, as `(user, channel)`.
    async fn voice_members(&self, guild_id: &str) -> Vec<(String, String)>;

    async fn join(&self, guild_id: &str, channel_id: &str) -> Result<()>;

    /// Plays `input`, replacing what's playing. Returns once playback has started (an input
    /// that fails right away is an error). May return a diagnostic note for the user.
    async fn play(&self, guild_id: &str, input: AudioInput) -> Result<Option<String>>;

    async fn stop(&self, guild_id: &str) -> Result<()>;

    /// Pauses or resumes what's playing. While paused, `is_playing` is false, so the idle timer
    /// still leaves after 10 minutes.
    async fn set_paused(&self, guild_id: &str, paused: bool) -> Result<()> {
        let _ = (guild_id, paused);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "pausing",
        })
    }

    /// Sets the volume in percent (0 to [`VOLUME_MAX`], 100 = unchanged) for what's playing and
    /// everything played later in this server.
    async fn set_volume(&self, guild_id: &str, percent: u16) -> Result<()> {
        let _ = (guild_id, percent);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "volume",
        })
    }

    async fn leave(&self, guild_id: &str) -> Result<()>;

    /// Whether the bot is currently in a voice channel in this server.
    async fn is_connected(&self, guild_id: &str) -> bool;

    /// Whether audio is playing right now in this server (for the idle timer).
    async fn is_playing(&self, guild_id: &str) -> bool;
}

/// Highest volume in percent. Above 100 the audio is amplified and may clip.
pub const VOLUME_MAX: u16 = 150;

/// Leave a voice channel after this long without playing anything.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const IDLE_CHECK_EVERY: Duration = Duration::from_secs(30);

/// Watches one server's voice connection and leaves after `IDLE_TIMEOUT` of silence. Exits when
/// the bot is no longer connected there. At most one watcher per (platform, server).
pub(crate) fn spawn_idle_watch(
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
        async fn play(&self, _: &str, _: AudioInput) -> Result<Option<String>> {
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
