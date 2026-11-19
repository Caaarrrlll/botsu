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
pub mod storage;

/// Permissions requested by the invite links logged at startup (Discord's permission bits, which
/// Fluxer mirrors): View Channels (1024) + Send Messages (2048) + Embed Links (16384, needed for
/// alert link previews) + Read Message History (65536). Grow this as features need more (phase 4:
/// Manage Messages, Manage Roles, Kick, Ban, Moderate Members; phase 6: Connect, Speak).
pub const INVITE_PERMISSIONS: u64 = 1024 | 2048 | 16384 | 65536;

pub use adapter::{Author, Capabilities, ChatAdapter, IncomingMessage, Platform};
pub use bot::Bot;
pub use command::{Command, CommandContext};
pub use error::{Error, Result};
