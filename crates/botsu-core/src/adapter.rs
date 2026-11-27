//! The boundary between the core and a chat platform.

use std::fmt;

use async_trait::async_trait;

use crate::error::{Error, Result};

/// Where a message came from or goes to. Each Fluxer instance botsu connects to is a platform of
/// its own, so IDs and settings never mix between instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// The first Fluxer instance: the first `FLUXER_URL` entry, or the official instance when
    /// `FLUXER_TOKEN` is given without a URL (as before several instances existed).
    Fluxer,
    Discord,
    /// The official Fluxer instance, through `FLUXER_OFFICIAL_TOKEN`.
    FluxerOfficial,
    /// Further `FLUXER_URL` entries: 2 to [`MAX_FLUXER_INSTANCES`].
    FluxerMore(u8),
}

/// Most self-hosted Fluxer instances (`FLUXER_URL` entries).
pub const MAX_FLUXER_INSTANCES: u8 = 9;

/// Storage keys of `FluxerMore(2..=9)`.
const FLUXER_MORE_KEYS: [&str; 8] = [
    "fluxer2", "fluxer3", "fluxer4", "fluxer5", "fluxer6", "fluxer7", "fluxer8", "fluxer9",
];

impl Platform {
    /// The self-hosted Fluxer instance at position `n` (1-based) in `FLUXER_URL`.
    pub fn fluxer_instance(n: u8) -> Option<Platform> {
        match n {
            1 => Some(Platform::Fluxer),
            2..=MAX_FLUXER_INSTANCES => Some(Platform::FluxerMore(n)),
            _ => None,
        }
    }

    /// The name stored in the database and export files. Fixed per instance, so data never
    /// moves to another instance when the list of instances changes.
    pub fn key(self) -> &'static str {
        match self {
            Platform::Discord => "discord",
            Platform::Fluxer => "fluxer",
            Platform::FluxerOfficial => "fluxer-official",
            Platform::FluxerMore(n) => FLUXER_MORE_KEYS
                .get(usize::from(n).wrapping_sub(2))
                .copied()
                .unwrap_or("fluxer-unknown"),
        }
    }

    pub fn from_key(key: &str) -> Option<Platform> {
        match key {
            "discord" => Some(Platform::Discord),
            "fluxer" => Some(Platform::Fluxer),
            "fluxer-official" => Some(Platform::FluxerOfficial),
            other => FLUXER_MORE_KEYS
                .iter()
                .position(|k| *k == other)
                .map(|i| Platform::FluxerMore(i as u8 + 2)),
        }
    }

    pub fn is_fluxer(self) -> bool {
        self != Platform::Discord
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Fluxer => f.write_str("Fluxer"),
            Platform::Discord => f.write_str("Discord"),
            Platform::FluxerOfficial => f.write_str("Fluxer (official)"),
            Platform::FluxerMore(n) => write!(f, "Fluxer {n}"),
        }
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
/// Manage Roles permission bit (Discord layout, also used by Fluxer).
pub const MANAGE_ROLES: u64 = 1 << 28;

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

/// A rich embed (alerts): a linked title, an author line, a large image and a colour bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Embed {
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    pub description: Option<String>,
    /// Shown large, under the text.
    pub image: Option<String>,
    pub color: Option<u32>,
    pub footer: Option<String>,
    /// Short `(name, value)` pairs shown side by side under the text.
    pub fields: Vec<(String, String)>,
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
    /// Files attached to the message (`admin import` reads one).
    pub attachments: Vec<Attachment>,
}

/// A file attached to an incoming message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub url: String,
    pub filename: String,
    /// Bytes, as reported by the platform (0 if unknown).
    pub size: u64,
}

/// A text channel in a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
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

    /// Sends `content` (may be empty) with one file attached and returns the message ID.
    async fn send_file(
        &self,
        channel_id: &str,
        content: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<String> {
        let _ = (channel_id, content, filename, data);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "file uploads",
        })
    }

    /// A server's text (and announcement) channels.
    async fn text_channels(&self, guild_id: &str) -> Result<Vec<ChannelInfo>> {
        let _ = guild_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "listing channels",
        })
    }

    /// Sends `content` (may be empty) with one embed and returns the message ID.
    async fn send_embed(&self, channel_id: &str, content: &str, embed: &Embed) -> Result<String> {
        let _ = (channel_id, content, embed);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "embeds",
        })
    }

    /// Sends a direct message with one file attached.
    async fn send_dm_file(
        &self,
        user_id: &str,
        content: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<()> {
        let _ = (user_id, content, filename, data);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "files by DM",
        })
    }

    /// A server's roles, channels and their role overrides, name, icon and banner
    /// (`admin structure export`).
    async fn read_structure(&self, guild_id: &str) -> Result<crate::structure::Structure> {
        let _ = guild_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "exporting a server's structure",
        })
    }

    /// Sets a server's name, and its icon and banner if given (image bytes).
    async fn set_guild_look(
        &self,
        guild_id: &str,
        name: &str,
        icon: Option<Vec<u8>>,
        banner: Option<Vec<u8>>,
    ) -> Result<()> {
        let _ = (guild_id, name, icon, banner);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "building a server's structure",
        })
    }

    /// Replaces the @everyone role's permissions (decimal string).
    async fn set_everyone_permissions(&self, guild_id: &str, permissions: &str) -> Result<()> {
        let _ = (guild_id, permissions);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "building a server's structure",
        })
    }

    /// Creates a role (below the existing ones) and returns its ID.
    async fn create_role(
        &self,
        guild_id: &str,
        role: &crate::structure::RoleSpec,
    ) -> Result<String> {
        let _ = (guild_id, role);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "building a server's structure",
        })
    }

    /// Creates a channel (after the existing ones in its category) with the given per-role
    /// overrides (already mapped to this server's role IDs) and returns its ID.
    async fn create_channel(
        &self,
        guild_id: &str,
        channel: &crate::structure::ChannelSpec,
        parent_id: Option<&str>,
        overwrites: &[crate::structure::Overwrite],
    ) -> Result<String> {
        let _ = (guild_id, channel, parent_id, overwrites);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "building a server's structure",
        })
    }

    /// Sends a direct message to a user (moderation notices). Fails if they don't accept DMs.
    async fn send_dm(&self, user_id: &str, content: &str) -> Result<()> {
        let _ = (user_id, content);
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "direct messages",
        })
    }

    /// A server's display name.
    async fn guild_name(&self, guild_id: &str) -> Result<String> {
        let _ = guild_id;
        Err(Error::Unsupported {
            platform: self.platform(),
            op: "guild_name",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_keys_round_trip_and_stay_fixed() {
        let all = [
            Platform::Discord,
            Platform::Fluxer,
            Platform::FluxerOfficial,
            Platform::FluxerMore(2),
            Platform::FluxerMore(9),
        ];
        for p in all {
            assert_eq!(Platform::from_key(p.key()), Some(p), "{p}");
        }
        // Existing data stays on the first instance.
        assert_eq!(Platform::Fluxer.key(), "fluxer");
        assert_eq!(Platform::fluxer_instance(1), Some(Platform::Fluxer));
        assert_eq!(Platform::fluxer_instance(3), Some(Platform::FluxerMore(3)));
        assert_eq!(Platform::fluxer_instance(10), None);
        assert_eq!(Platform::from_key("fluxer10"), None);
        assert_eq!(Platform::FluxerOfficial.to_string(), "Fluxer (official)");
    }
}
