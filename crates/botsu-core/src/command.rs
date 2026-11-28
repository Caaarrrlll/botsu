//! Prefix commands.
//!
//! Fluxer (via fluxer-rust 0.2) has no slash commands/interactions, so prefix
//! commands are the common denominator. Discord slash commands can be layered
//! on later by having the Discord adapter build the same [`CommandContext`].

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::adapter::{ChatAdapter, IncomingMessage, Platform};
use crate::error::{Error, Result};
use crate::permissions::Level;

pub struct CommandContext {
    pub chat: Arc<dyn ChatAdapter>,
    pub message: IncomingMessage,
    /// Everything after the command name, trimmed.
    pub args: String,
    /// The author is listed in `BOTSU_OWNERS`.
    pub is_owner: bool,
    /// What to type before this command's name in usage hints: the bot prefix, plus the group
    /// for grouped commands (`"!mod "` while running `!mod ban`).
    pub prefix: String,
    /// The bot's own command prefix (`"!"`), for recognising commands in chat.
    pub bot_prefix: String,
    /// The author's permission level. Exact for commands whose `level()` is above
    /// `Everyone` (the bot works it out before running them); otherwise `Everyone`, or
    /// `BotOwner` for bot owners.
    pub level: Level,
}

impl CommandContext {
    /// Refuses channels from other servers: a channel ID typed in one server must not let a
    /// command post or act in another server the bot is in.
    pub async fn ensure_channel_here(&self, channel_id: &str) -> Result<()> {
        if channel_id == self.message.channel_id {
            return Ok(());
        }
        let not_here = || Error::Usage(format!("<#{channel_id}> isn't a channel in this server."));
        let Some(guild) = self.message.guild_id.as_deref() else {
            return Err(not_here());
        };
        match self.chat.channel_guild(channel_id).await {
            Ok(Some(g)) if g == guild => Ok(()),
            Ok(_) => Err(not_here()),
            Err(e) => {
                tracing::info!(error = %e, channel = channel_id, "channel lookup failed");
                Err(not_here())
            }
        }
    }

    /// Replies in the channel the command came from.
    pub async fn reply(&self, content: &str) -> Result<String> {
        self.chat
            .send_message(&self.message.channel_id, content)
            .await
    }

    /// DMs `text` (and a file) to whoever ran the command and says so briefly in the channel,
    /// for replies that shouldn't be posted where everyone sees them (exports, previews). If the
    /// DM can't be sent, the error says why.
    pub async fn reply_dm(&self, text: &str, file: Option<(&str, Vec<u8>)>) -> Result<()> {
        let user = &self.message.author.id;
        let sent = match file {
            Some((name, data)) => self.chat.send_dm_file(user, text, name, data).await,
            None => self.chat.send_dm(user, text).await,
        };
        match sent {
            Ok(()) => {
                self.reply("Sent you a DM.").await?;
                Ok(())
            }
            Err(Error::Unsupported { .. }) => Err(Error::Usage(
                "I can't send that by DM on this platform.".into(),
            )),
            Err(e) => {
                tracing::info!(error = %e, "DM failed");
                Err(Error::Usage(
                    "I couldn't DM you. Allow direct messages from server members and try again."
                        .into(),
                ))
            }
        }
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

    /// Minimum level to run the command at all (and to see it in `help`). Commands with
    /// mixed subcommands keep `Everyone` and check inside.
    fn level(&self) -> Level {
        Level::Everyone
    }

    /// Other names that run this command (not listed in `help`).
    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    /// The bot works out the caller's exact level before running the command even when
    /// `level()` is `Everyone` (groups whose subcommands need more).
    fn needs_level(&self) -> bool {
        false
    }

    /// Subcommands `level` may run, as `(name, help)`, for `help`. Only groups have any.
    fn subcommands(&self, level: Level) -> Vec<(&'static str, &'static str)> {
        let _ = level;
        Vec::new()
    }

    async fn run(&self, ctx: CommandContext) -> Result<()>;
}

/// Commands grouped under one name, like `!mod ban …` and `!admin addrole …`. Each subcommand
/// keeps its own level; the group shows up in `help` for anyone who may run at least one of
/// them, and `!<group>` alone lists the ones the caller may use.
pub struct Group {
    name: &'static str,
    help: &'static str,
    title: &'static str,
    aliases: Vec<&'static str>,
    subs: BTreeMap<&'static str, Arc<dyn Command>>,
}

impl Group {
    pub fn new(name: &'static str, title: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            title,
            aliases: Vec::new(),
            subs: BTreeMap::new(),
        }
    }

    pub fn alias(mut self, alias: &'static str) -> Self {
        self.aliases.push(alias);
        self
    }

    pub fn with(mut self, command: impl Command + 'static) -> Self {
        self.subs.insert(command.name(), Arc::new(command));
        self
    }

    fn listing(&self, ctx: &CommandContext) -> String {
        let subs: Vec<(&str, &str)> = self
            .subs
            .values()
            .filter(|c| c.level() <= ctx.level)
            .map(|c| (c.name(), c.help()))
            .collect();
        usage_block(self.title, &format!("{}{}", ctx.prefix, self.name), &subs)
    }
}

/// A command's subcommands as a code block, laid out like `help`: `**title**`, then one
/// aligned `<command> <sub>  <what it does>` line each.
pub fn usage_block(title: &str, command: &str, subs: &[(&str, &str)]) -> String {
    let width = subs
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!("**{title}**\n```\n");
    for (sub, help) in subs {
        out.push_str(&format!(
            "{command} {sub:<width$}  {}\n",
            help.replace('`', "")
        ));
    }
    out.push_str("```");
    out
}

#[async_trait]
impl Command for Group {
    fn name(&self) -> &'static str {
        self.name
    }

    fn help(&self) -> &'static str {
        self.help
    }

    fn level(&self) -> Level {
        self.subs
            .values()
            .map(|c| c.level())
            .min()
            .unwrap_or(Level::Everyone)
    }

    fn aliases(&self) -> &[&'static str] {
        &self.aliases
    }

    fn needs_level(&self) -> bool {
        self.subs.values().any(|c| c.level() > Level::Everyone)
    }

    fn subcommands(&self, level: Level) -> Vec<(&'static str, &'static str)> {
        self.subs
            .values()
            .filter(|c| c.level() <= level)
            .map(|c| (c.name(), c.help()))
            .collect()
    }

    async fn run(&self, mut ctx: CommandContext) -> Result<()> {
        let args = ctx.args.clone();
        let (sub, rest) = match args.split_once(char::is_whitespace) {
            Some((s, r)) => (s.to_lowercase(), r.trim().to_owned()),
            None => (args.to_lowercase(), String::new()),
        };
        if sub.is_empty() || sub == "help" {
            ctx.reply(&self.listing(&ctx)).await?;
            return Ok(());
        }
        let Some(cmd) = self.subs.get(sub.as_str()).cloned() else {
            return Err(Error::Usage(format!(
                "There's no `{sub}` here.\n{}",
                self.listing(&ctx)
            )));
        };
        if ctx.level < cmd.level() {
            return Err(Error::Usage(format!(
                "You need to be {} to use `{}{} {sub}`.",
                cmd.level(),
                ctx.prefix,
                self.name
            )));
        }
        ctx.prefix = format!("{}{} ", ctx.prefix, self.name);
        ctx.args = rest;
        cmd.run(ctx).await
    }
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

/// Where people report bugs and ask for features.
pub const ISSUES_URL: &str = "https://github.com/Caaarrrlll/botsu/issues";

/// `feedback` (aliases `bug`, `feature`, `issue`): where to report a bug or ask for a feature.
pub(crate) struct Feedback;

#[async_trait]
impl Command for Feedback {
    fn name(&self) -> &'static str {
        "feedback"
    }

    fn aliases(&self) -> &[&'static str] {
        &["bug", "feature", "issue"]
    }

    fn help(&self) -> &'static str {
        "Where to report a bug or ask for a feature."
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        ctx.reply(&format!(
            "Found a bug or want a feature? Open an issue (a GitHub account is needed): \
             <{ISSUES_URL}/new>\nExisting ones: <{ISSUES_URL}>"
        ))
        .await?;
        Ok(())
    }
}

/// What `invite` shows. The adapters add the links when they connect
/// ([`crate::Bot::register_invite`]).
#[derive(Debug, Default)]
pub(crate) struct InviteInfo {
    /// This bot's links, one per connected platform, sorted by platform name.
    pub own: Vec<(Platform, String)>,
    /// This is the experimental build (`BOTSU_CHANNEL`).
    pub experimental: bool,
    /// The prefix in use (doubled on the experimental build).
    pub prefix: String,
}

pub(crate) type InviteLinks = Arc<RwLock<InviteInfo>>;

/// The `invite` reply for the running build. Links in <...> so the apps don't show a preview for
/// each.
pub(crate) fn invite_text(info: &InviteInfo) -> String {
    if info.own.is_empty() {
        return "No platform is connected yet, so there's no invite link.".to_owned();
    }
    let links: Vec<String> = info
        .own
        .iter()
        .map(|(platform, url)| format!("• {platform}: <{url}>"))
        .collect();
    let intro = if info.experimental {
        format!(
            "Add **botsu experimental** to a server. This is the experimental build: it gets new \
             features first and may be unstable. Its commands start with `{}`.",
            info.prefix
        )
    } else {
        "Add botsu to a server:".to_owned()
    };
    format!("{intro}\n{}", links.join("\n"))
}

/// `invite`: the links to add this bot to a server.
pub(crate) struct Invite(pub(crate) InviteLinks);

#[async_trait]
impl Command for Invite {
    fn name(&self) -> &'static str {
        "invite"
    }

    fn help(&self) -> &'static str {
        "Links to add the bot to a server, one per platform."
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let text = invite_text(&self.0.read().unwrap_or_else(|p| p.into_inner()));
        ctx.reply(&text).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn invite_for_the_running_build() {
        use super::{invite_text, InviteInfo};
        use crate::adapter::Platform;
        let mut info = InviteInfo {
            own: vec![
                (
                    Platform::Discord,
                    "https://discord.com/oauth2/authorize?client_id=1".into(),
                ),
                (
                    Platform::Fluxer,
                    "https://fluxer.example.org/oauth2/authorize?client_id=1".into(),
                ),
            ],
            experimental: false,
            prefix: "$".into(),
        };
        let text = invite_text(&info);
        assert!(
            text.starts_with("Add botsu to a server:\n• Discord: <https://discord.com/"),
            "{text}"
        );
        assert!(!text.contains("unstable"));

        info.experimental = true;
        info.prefix = "$$".into();
        let text = invite_text(&info);
        assert!(
            text.contains("experimental build") && text.contains("may be unstable"),
            "{text}"
        );
        assert!(text.contains("`$$`") && text.contains("• Fluxer: <https://fluxer.example.org/"));

        assert_eq!(
            invite_text(&InviteInfo::default()),
            "No platform is connected yet, so there's no invite link."
        );
    }

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
