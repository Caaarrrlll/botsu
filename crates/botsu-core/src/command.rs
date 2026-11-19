//! Prefix commands.
//!
//! Fluxer (via fluxer-rust 0.2) has no slash commands/interactions, so prefix
//! commands are the common denominator. Discord slash commands can be layered
//! on later by having the Discord adapter build the same [`CommandContext`].

use std::sync::Arc;

use async_trait::async_trait;

use crate::adapter::{ChatAdapter, IncomingMessage};
use crate::error::{Error, Result};

pub struct CommandContext {
    pub chat: Arc<dyn ChatAdapter>,
    pub message: IncomingMessage,
    /// Everything after the command name, trimmed.
    pub args: String,
    /// The author is listed in `BOTSU_OWNERS`.
    pub is_owner: bool,
    pub prefix: String,
}

impl CommandContext {
    /// Replies in the channel the command came from.
    pub async fn reply(&self, content: &str) -> Result<String> {
        self.chat
            .send_message(&self.message.channel_id, content)
            .await
    }

    /// Stops the command unless the author is a bot owner. The reply includes the author's
    /// ID, so setting up `BOTSU_OWNERS` doesn't need developer mode to find it.
    pub fn require_owner(&self) -> Result<()> {
        if self.is_owner {
            return Ok(());
        }
        Err(Error::Usage(format!(
            "Only bot owners can do that. (Your {} user ID is `{}`; the bot's host can add it \
             to BOTSU_OWNERS.)",
            self.message.platform, self.message.author.id
        )))
    }
}

#[async_trait]
pub trait Command: Send + Sync {
    /// The name typed after the prefix, lower case.
    fn name(&self) -> &'static str;

    /// One line for `help`.
    fn help(&self) -> &'static str;

    async fn run(&self, ctx: CommandContext) -> Result<()>;
}

/// Splits `"$play some song"` into `("play", "some song")` if it starts with `prefix`.
/// The command name is lower-cased so `$PING` works too.
pub fn parse<'a>(prefix: &str, content: &'a str) -> Option<(String, &'a str)> {
    let rest = content.strip_prefix(prefix)?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (rest, ""),
    };
    if name.is_empty() {
        return None;
    }
    Some((name.to_lowercase(), args))
}

pub(crate) struct Ping;

#[async_trait]
impl Command for Ping {
    fn name(&self) -> &'static str {
        "ping"
    }

    fn help(&self) -> &'static str {
        "Check that the bot is alive."
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        ctx.reply(&format!("Pong! ({})", ctx.chat.platform()))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_name_and_args() {
        assert_eq!(
            parse("$", "$play  never gonna"),
            Some(("play".into(), "never gonna"))
        );
        assert_eq!(parse("$", "$PING"), Some(("ping".into(), "")));
    }

    #[test]
    fn ignores_non_commands() {
        assert_eq!(parse("$", "hello"), None);
        assert_eq!(parse("$", "$"), None);
        assert_eq!(parse("$", "$ ping"), None);
    }

    #[test]
    fn bang_prefix_works_alongside_red() {
        assert_eq!(parse("!", "!ping"), Some(("ping".into(), "")));
        // Red's commands must not trigger botsu while botsu uses '!'.
        assert_eq!(parse("!", "$ping"), None);
        // Exclamations in normal chat aren't commands.
        assert_eq!(parse("!", "! wow"), None);
        assert_eq!(parse("!", "nice!"), None);
    }

    #[test]
    fn multi_char_prefix() {
        assert_eq!(parse("b!", "b!help"), Some(("help".into(), "")));
        assert_eq!(parse("b!", "b ping"), None);
    }
}
