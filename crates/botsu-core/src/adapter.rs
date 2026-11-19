//! The boundary between the core and a chat platform.

use std::fmt;

use async_trait::async_trait;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Fluxer,
    Discord,
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Platform::Fluxer => "Fluxer",
            Platform::Discord => "Discord",
        })
    }
}

/// What an adapter can do. The core checks this before offering a feature,
/// so a command can say "not available here" instead of failing halfway.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub bulk_delete: bool,
    pub slash_commands: bool,
    pub voice: bool,
}

#[derive(Debug, Clone)]
pub struct Author {
    pub id: String,
    pub name: String,
    pub is_bot: bool,
}

/// A chat message, already translated out of the platform's types.
/// IDs are strings: every platform we target uses string snowflakes or ULIDs.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub platform: Platform,
    pub guild_id: Option<String>,
    pub channel_id: String,
    pub message_id: String,
    pub author: Author,
    pub content: String,
}

/// Outbound operations the core may ask of a platform.
///
/// Methods with default bodies are optional: an adapter overrides the ones its
/// platform supports, and the default reports [`Error::Unsupported`].
/// Grow this trait as features need it (roles, kick/ban, timeouts in phase 3).
#[async_trait]
pub trait ChatAdapter: Send + Sync {
    fn platform(&self) -> Platform;

    fn capabilities(&self) -> Capabilities;

    /// Sends a plain-text message and returns the new message's ID.
    async fn send_message(&self, channel_id: &str, content: &str) -> Result<String>;

    async fn delete_message(&self, channel_id: &str, message_id: &str) -> Result<()> {
        let _ = (channel_id, message_id);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "delete_message",
        })
    }

    async fn bulk_delete(&self, channel_id: &str, message_ids: &[String]) -> Result<()> {
        let _ = (channel_id, message_ids);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "bulk_delete",
        })
    }
}
