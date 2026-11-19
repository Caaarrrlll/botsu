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

/// The Administrator permission bit. Discord and Fluxer use the same permission bit layout.
pub const ADMINISTRATOR: u64 = 1 << 3;

/// A server role.
#[derive(Debug, Clone)]
pub struct RoleInfo {
    pub id: String,
    pub name: String,
    /// Permission bits (Discord layout).
    pub permissions: u64,
    /// Rank in the role list; higher ranks higher. @everyone is 0.
    pub position: i64,
}

/// A message from a channel's history (for cleanup).
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub id: String,
    pub author_id: String,
    pub author_is_bot: bool,
    pub content: String,
    /// Unix seconds; 0 if the platform didn't say.
    pub timestamp: i64,
}

/// An entry of a server's ban list.
#[derive(Debug, Clone)]
pub struct BanInfo {
    pub user_id: String,
    pub user_name: String,
    pub reason: Option<String>,
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
/// Grow this trait as features need it.
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

    /// The server a channel belongs to (None for DMs). Used to refuse channel IDs from other
    /// servers the bot is in.
    async fn channel_guild(&self, channel_id: &str) -> Result<Option<String>> {
        let _ = channel_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "channel_guild",
        })
    }

    /// Role IDs of a server member.
    async fn member_roles(&self, guild_id: &str, user_id: &str) -> Result<Vec<String>> {
        let _ = (guild_id, user_id);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "member_roles",
        })
    }

    /// All roles of a server.
    async fn roles(&self, guild_id: &str) -> Result<Vec<RoleInfo>> {
        let _ = guild_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "roles",
        })
    }

    async fn add_role(&self, guild_id: &str, user_id: &str, role_id: &str) -> Result<()> {
        let _ = (guild_id, user_id, role_id);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "add_role",
        })
    }

    async fn remove_role(&self, guild_id: &str, user_id: &str, role_id: &str) -> Result<()> {
        let _ = (guild_id, user_id, role_id);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "remove_role",
        })
    }

    /// Up to `limit` (≤ 100) messages before `before` (or the newest), newest first.
    async fn recent_messages(
        &self,
        channel_id: &str,
        before: Option<&str>,
        limit: u8,
    ) -> Result<Vec<HistoryMessage>> {
        let _ = (channel_id, before, limit);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "recent_messages",
        })
    }

    /// The user ID of a server's owner, for "server owner may do this" permission checks.
    async fn guild_owner(&self, guild_id: &str) -> Result<Option<String>> {
        let _ = guild_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "guild_owner",
        })
    }

    /// Finds a channel in a server by name (case-insensitive, without `#`). Used when
    /// importing lists that name channels instead of linking them.
    async fn find_channel(&self, guild_id: &str, name: &str) -> Result<Option<String>> {
        let _ = (guild_id, name);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "find_channel",
        })
    }

    async fn bulk_delete(&self, channel_id: &str, message_ids: &[String]) -> Result<()> {
        let _ = (channel_id, message_ids);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "bulk_delete",
        })
    }

    // --- Moderation (phase 5). Reasons go to the platform's audit log. ---

    /// The bot's own user ID.
    async fn bot_user_id(&self) -> Result<String> {
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "bot_user_id",
        })
    }

    /// A user's display name (for mod-log posts, which shouldn't ping anyone).
    async fn user_name(&self, user_id: &str) -> Result<String> {
        let _ = user_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "user_name",
        })
    }

    async fn kick(&self, guild_id: &str, user_id: &str, reason: Option<&str>) -> Result<()> {
        let _ = (guild_id, user_id, reason);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "kick",
        })
    }

    /// Bans a user (member or not), deleting their messages from the last
    /// `delete_message_days` (0–7) days.
    async fn ban(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
        delete_message_days: u8,
    ) -> Result<()> {
        let _ = (guild_id, user_id, reason, delete_message_days);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "ban",
        })
    }

    async fn unban(&self, guild_id: &str, user_id: &str, reason: Option<&str>) -> Result<()> {
        let _ = (guild_id, user_id, reason);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "unban",
        })
    }

    async fn bans(&self, guild_id: &str) -> Result<Vec<BanInfo>> {
        let _ = guild_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "bans",
        })
    }

    /// Times a member out until `until` (unix seconds); `None` lifts the timeout.
    async fn timeout(
        &self,
        guild_id: &str,
        user_id: &str,
        until: Option<i64>,
        reason: Option<&str>,
    ) -> Result<()> {
        let _ = (guild_id, user_id, until, reason);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "timeout",
        })
    }
}
