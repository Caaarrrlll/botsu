//! Voice: the platform-neutral interface (Discord: songbird; Fluxer: LiveKit) and the `voice`
//! test command. Phase 0 uses it to check that both platforms can play audio; phase 6 (music)
//! builds the queue on top of the same interface.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::adapter::Platform;
use crate::command::{Command, CommandContext};
use crate::error::{Error, Result};

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
    async fn play(&self, guild_id: &str, source: &str) -> Result<()>;

    async fn stop(&self, guild_id: &str) -> Result<()>;

    async fn leave(&self, guild_id: &str) -> Result<()>;
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

/// `voice join|play|stop|leave|where`: manual voice test, bot owners only.
pub struct VoiceCommand {
    adapters: Vec<Arc<dyn VoiceAdapter>>,
}

impl VoiceCommand {
    pub fn new(adapters: Vec<Arc<dyn VoiceAdapter>>) -> Self {
        Self { adapters }
    }

    fn usage(p: &str) -> String {
        format!(
            "Voice test (bot owners):\n\
             `{p}voice join [channel]`: join your voice channel, or the given one\n\
             `{p}voice play [url]`: play a 10-second test tone, or a link/file\n\
             `{p}voice stop` / `{p}voice leave`\n\
             `{p}voice where`: who I can see in voice here"
        )
    }
}

#[async_trait]
impl Command for VoiceCommand {
    fn name(&self) -> &'static str {
        "voice"
    }

    fn help(&self) -> &'static str {
        "Voice test: join, play, stop, leave, where (bot owners)."
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
                op: "voice",
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
                         or use `{}voice join <channel id>`.",
                        ctx.prefix
                    )));
                };
                ctx.reply(&format!("Joining <#{target}>...")).await?;
                voice.join(&guild, &target).await?;
                ctx.reply(&format!("Joined. Try `{}voice play`.", ctx.prefix))
                    .await?;
            }
            "play" => {
                let source = if arg.is_empty() {
                    test_tone().await?.to_string_lossy().into_owned()
                } else {
                    arg.trim_start_matches('<').trim_end_matches('>').to_owned()
                };
                voice.play(&guild, &source).await?;
                let shown = if Path::new(&source)
                    .file_name()
                    .is_some_and(|n| n == "botsu-test-tone.wav")
                {
                    "the test tone (10 s, 440 Hz)".to_owned()
                } else {
                    format!("<{source}>")
                };
                ctx.reply(&format!("Playing {shown}.")).await?;
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
