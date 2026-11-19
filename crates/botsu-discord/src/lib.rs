//! Discord adapter, built on serenity.
//!
//! This is the only crate allowed to touch serenity types.

use std::sync::Arc;

use botsu_core::{Author, Bot, Capabilities, ChatAdapter, Error, IncomingMessage, Platform};
use serenity::all::{
    ChannelId, Client, Context, EventHandler, GatewayIntents, Http, Message, MessageId, Ready,
};
use serenity::async_trait;

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
            // songbird isn't wired in yet (phase 6).
            voice: false,
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

/// Inbound side: receives gateway events and forwards them to the core.
struct Handler {
    bot: Arc<Bot>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        tracing::info!(
            user = %ready.user.name,
            guilds = ready.guilds.len(),
            "connected to Discord"
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
pub async fn run(config: DiscordConfig, bot: Arc<Bot>) -> Result<(), Error> {
    // MESSAGE_CONTENT is a privileged intent: enable it in the developer portal
    // (Bot → Privileged Gateway Intents) or prefix commands won't see any text.
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&config.token, intents)
        .event_handler(Handler { bot })
        .await
        .map_err(err)?;
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
