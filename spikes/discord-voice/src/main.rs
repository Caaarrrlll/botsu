//! Phase 0 spike: can a bot join Discord voice (DAVE end-to-end encryption) and play audio with
//! songbird? Follows you into your voice channel using serenity's cache of voice states.
//!
//! Run from this folder: `cargo run`. Reads DISCORD_TOKEN from ../../.env.
//! Commands (prefix !): join [channel id], play [file], stop, leave, where.
//! More detail in the log: set RUST_LOG=info,songbird=debug before running.

use std::path::Path;

use serenity::all::{ChannelId, Client, Context, EventHandler, GatewayIntents, Message, Ready};
use serenity::async_trait;
use songbird::input::File;
use songbird::SerenityInit;

const TONE: &str = "../test-tone.mp3";

struct Handler;

async fn say(ctx: &Context, msg: &Message, text: &str) {
    println!("> {text}");
    let _ = msg.channel_id.say(&ctx.http, text).await;
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("Logged in as {}", ready.user.name);
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }
        let Some(guild_id) = msg.guild_id else { return };
        let Some(rest) = msg.content.strip_prefix('!') else { return };
        let (cmd, arg) = rest.split_once(' ').map_or((rest, ""), |(c, a)| (c, a.trim()));
        let manager = songbird::get(&ctx).await.expect("songbird registered");

        match cmd {
            "where" => {
                // Copy out of the cache before any await.
                let lines: Vec<String> = ctx
                    .cache
                    .guild(guild_id)
                    .map(|g| {
                        g.voice_states
                            .values()
                            .filter_map(|vs| vs.channel_id.map(|c| format!("user {} in {}", vs.user_id, c)))
                            .collect()
                    })
                    .unwrap_or_default();
                let text = if lines.is_empty() {
                    "I can't see anyone in a voice channel here.".to_owned()
                } else {
                    lines.join("\n")
                };
                say(&ctx, &msg, &text).await;
            }
            "join" => {
                let target = if arg.is_empty() {
                    ctx.cache.guild(guild_id).and_then(|g| {
                        g.voice_states
                            .get(&msg.author.id)
                            .and_then(|vs| vs.channel_id)
                    })
                } else {
                    arg.trim_start_matches("<#")
                        .trim_end_matches('>')
                        .parse::<u64>()
                        .ok()
                        .map(ChannelId::new)
                };
                let Some(target) = target else {
                    say(&ctx, &msg, "I can't see you in a voice channel. Join one first, or use `!join <channel id>`.").await;
                    return;
                };
                say(&ctx, &msg, &format!("Joining <#{target}>...")).await;
                match manager.join(guild_id, target).await {
                    Ok(_) => say(&ctx, &msg, "Joined. Try `!play`.").await,
                    Err(e) => say(&ctx, &msg, &format!("Join failed: {e:?}")).await,
                }
            }
            "play" => {
                let path = if arg.is_empty() { TONE } else { arg };
                let Some(call) = manager.get(guild_id) else {
                    say(&ctx, &msg, "Not in a voice channel; `!join` first.").await;
                    return;
                };
                let mut call = call.lock().await;
                call.stop();
                call.play_input(File::new(path.to_owned()).into());
                drop(call);
                say(&ctx, &msg, &format!("Playing `{path}`.")).await;
            }
            "stop" => {
                if let Some(call) = manager.get(guild_id) {
                    call.lock().await.stop();
                }
                say(&ctx, &msg, "Stopped.").await;
            }
            "leave" => {
                let _ = manager.remove(guild_id).await;
                say(&ctx, &msg, "Left.").await;
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

/// A 10-second 440 Hz test tone, made with ffmpeg the first time (shared with the Fluxer spike).
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
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set (../../.env)");

    tokio::runtime::Runtime::new().unwrap().block_on(async move {
        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_VOICE_STATES
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;
        let mut client = Client::builder(&token, intents)
            .event_handler(Handler)
            .register_songbird()
            .await
            .expect("client");
        if let Err(e) = client.start().await {
            eprintln!("Error: {e}");
        }
    });
}
