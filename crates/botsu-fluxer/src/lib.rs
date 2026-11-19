//! Fluxer adapter, built on `fluxer-rust` (library name `fluxer`).
//!
//! This is the only crate allowed to touch fluxer-rust types. When a Fluxer or
//! fluxer-rust update breaks something, the fix should stay inside this crate.

use std::sync::Arc;

use async_trait::async_trait;
use botsu_core::{Author, Bot, Capabilities, ChatAdapter, Error, IncomingMessage, Platform};
use fluxer::http::Http;
use fluxer::prelude::{Client, Context, EventHandler, Message, Ready};

/// Default public instance. Self-hosted instances set `FLUXER_API_URL`.
pub const DEFAULT_API_URL: &str = "https://api.fluxer.app/v1";

#[derive(Debug, Clone)]
pub struct FluxerConfig {
    pub token: String,
    pub api_url: String,
}

/// Outbound side: implements the core's [`ChatAdapter`] with Fluxer REST calls.
pub struct FluxerChat {
    http: Arc<Http>,
}

impl FluxerChat {
    pub fn new(config: &FluxerConfig) -> Self {
        Self {
            http: Arc::new(Http::new(&config.token, config.api_url.clone())),
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
            // LiveKit voice exists in fluxer-rust but isn't wired into the core yet (phase 4).
            voice: false,
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

/// Inbound side: receives gateway events and forwards them to the core.
struct Handler {
    bot: Arc<Bot>,
    chat: Arc<dyn ChatAdapter>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn on_ready(&self, _ctx: Context, ready: Ready) {
        tracing::info!(user = %ready.user.username, "connected to Fluxer");
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
pub async fn run(config: FluxerConfig, bot: Arc<Bot>) -> Result<(), Error> {
    let fluxer_chat = FluxerChat::new(&config);
    preflight(&fluxer_chat.http, &config.api_url).await?;

    let chat: Arc<dyn ChatAdapter> = Arc::new(fluxer_chat);
    let mut client = Client::builder(&config.token)
        .api_url(&config.api_url)
        .event_handler(Handler { bot, chat })
        .build();
    client.start().await.map_err(err)
}

/// Fails fast with a readable error if the token or API URL is wrong, instead
/// of letting the gateway loop retry forever. Also logs what the instance
/// advertises (useful while checking a self-hosted setup).
async fn preflight(http: &Http, api_url: &str) -> Result<(), Error> {
    match http.get_well_known_fluxer().await {
        Ok(wk) => tracing::info!(
            api = %wk.endpoints.api,
            gateway = %wk.endpoints.gateway,
            voice_enabled = wk.features.voice_enabled,
            self_hosted = wk.features.self_hosted,
            "Fluxer instance discovered"
        ),
        // Not fatal: older or customised instances may not serve it.
        Err(e) => tracing::warn!(error = %e, "could not read /.well-known/fluxer"),
    }

    let me = http.get_me().await.map_err(|e| {
        err(format!(
            "login check failed against {api_url} ({e}); check FLUXER_TOKEN and FLUXER_API_URL"
        ))
    })?;
    tracing::info!(user = %me.username, id = %me.id, "Fluxer token OK");
    Ok(())
}
