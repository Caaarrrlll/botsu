//! Platform-neutral core of botsu.
//!
//! Adapters (Fluxer, later Discord) translate platform events into
//! [`IncomingMessage`]s and hand them to [`Bot::handle_message`]. Everything the
//! core needs to do *to* a platform goes through the [`ChatAdapter`] trait.
//!
//! Feature modules (alerts, moderation, music) will live here as they are
//! built; see `docs/handover.md` section 8 for the phase order.

pub mod adapter;
pub mod bot;
pub mod command;
pub mod error;

pub use adapter::{Author, Capabilities, ChatAdapter, IncomingMessage, Platform};
pub use bot::Bot;
pub use command::{Command, CommandContext};
pub use error::{Error, Result};
