//! Platform-neutral core of botsu.
//!
//! Adapters (Fluxer, later Discord) translate platform events into
//! [`IncomingMessage`]s and hand them to [`Bot::handle_message`]. Everything the
//! core needs to do *to* a platform goes through the [`ChatAdapter`] trait.
//!
//! Feature modules (alerts, moderation, music) will live here as they are
//! built; see `docs/handover.md` section 8 for the phase order.

pub mod adapter;
pub mod alerts;
pub mod bot;
pub mod command;
pub mod error;
pub mod moderation;
pub mod music;
pub mod permissions;
pub mod server;
pub mod storage;
pub mod transfer;
pub mod voice;
pub mod ytdlp;

/// Permissions requested by the invite links logged at startup (Discord's permission bits, which
/// Fluxer mirrors): View Channels (1024) + Send Messages (2048) + Embed Links (16384, needed for
/// alert link previews) + Manage Messages (1 << 13, cleanup) + Read Message History (65536) +
/// Manage Roles (1 << 28, addrole/removerole) + Connect (1 << 20) + Speak (1 << 21) for
/// voice + Kick Members (1 << 1) + Ban Members (1 << 2) + Moderate Members (1 << 40, timeouts).
pub const INVITE_PERMISSIONS: u64 = 1024
    | 2048
    | (1 << 13)
    | 16384
    | 65536
    | (1 << 28)
    | (1 << 20)
    | (1 << 21)
    | (1 << 1)
    | (1 << 2)
    | (1 << 40);

/// Administrator: everything, and channel overrides no longer apply. Role hierarchy still does.
pub const ADMIN_PERMISSIONS: u64 = 1 << 3;

/// What the startup invite links request: the list above, or Administrator when the host set
/// `BOTSU_INVITE_ADMIN` (a leaked token then controls every server that accepted it).
pub fn invite_permissions(admin: bool) -> u64 {
    if admin {
        ADMIN_PERMISSIONS
    } else {
        INVITE_PERMISSIONS
    }
}

pub use adapter::{
    Attachment, Author, BanInfo, Capabilities, ChannelInfo, ChatAdapter, Embed, HistoryMessage,
    IncomingMessage, Platform, RoleInfo,
};
pub use bot::Bot;
pub use command::{Command, CommandContext};
pub use error::{Error, Result};
