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
pub mod permissions;
pub mod server;
pub mod storage;
pub mod voice;

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

pub use adapter::{
    Author, BanInfo, Capabilities, ChatAdapter, HistoryMessage, IncomingMessage, Platform, RoleInfo,
};
pub use bot::Bot;
pub use command::{Command, CommandContext};
pub use error::{Error, Result};
