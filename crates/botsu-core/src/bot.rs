use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::adapter::{ChatAdapter, IncomingMessage, Platform};
use crate::command::{self, Command, CommandContext, Feedback, Invite, InviteLinks, Ping};
use crate::error::Error;
use crate::permissions::{Level, Permissions};

/// The shared, platform-neutral bot. One instance serves every adapter.
pub struct Bot {
    prefix: String,
    /// User IDs (any platform) with every permission (`BOTSU_OWNERS`).
    owners: HashSet<String>,
    commands: BTreeMap<&'static str, Arc<dyn Command>>,
    /// Works out admin/mod levels; without it only bot owners are above "everyone".
    permissions: Option<Arc<Permissions>>,
    /// Connected platforms, for sending outside a command (alerts).
    chats: RwLock<HashMap<Platform, Arc<dyn ChatAdapter>>>,
    /// Invite links of the connected platforms, shared with the `invite` command.
    invites: InviteLinks,
    /// Where servers' own prefixes (`setprefix`) are stored; `None`: the default everywhere.
    store: Option<Arc<crate::storage::Store>>,
    /// Where custom commands (`cc`) are stored; `None`: custom commands off.
    custom: Option<Arc<crate::storage::Store>>,
}

impl Bot {
    pub fn new(prefix: impl Into<String>, owners: impl IntoIterator<Item = String>) -> Self {
        let mut bot = Self {
            prefix: prefix.into(),
            owners: owners.into_iter().collect(),
            commands: BTreeMap::new(),
            permissions: None,
            chats: RwLock::default(),
            invites: InviteLinks::default(),
            store: None,
            custom: None,
        };
        bot.register(Ping);
        bot.invites
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .base_prefix = bot.prefix.clone();
        bot.register(Invite(bot.invites.clone()));
        bot.register(Feedback);
        bot
    }

    pub fn set_permissions(&mut self, permissions: Arc<Permissions>) {
        self.permissions = Some(permissions);
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Per-server prefixes: adds the `setprefix` command (mods). The prefix is read from the
    /// database for each message (a primary-key lookup), so an import or `setprefix` applies
    /// straight away.
    pub fn enable_server_prefixes(&mut self, store: Arc<crate::storage::Store>) {
        self.register(crate::server::SetPrefixCommand::new(
            store.clone(),
            self.prefix.clone(),
        ));
        self.store = Some(store);
    }

    /// Custom commands (`cc`). Call after registering every built-in command: their names
    /// (and `help`) are reserved.
    pub fn enable_custom_commands(&mut self, store: Arc<crate::storage::Store>) {
        let reserved = self.reserved_names();
        self.register(crate::custom::CustomCommand::new(store.clone(), reserved));
        self.custom = Some(store);
    }

    /// Names a custom command can't take: every registered command and alias, `help`, and
    /// `cc` itself.
    fn reserved_names(&self) -> std::collections::BTreeSet<String> {
        let mut reserved: std::collections::BTreeSet<String> =
            self.commands.keys().map(|k| (*k).to_owned()).collect();
        for name in ["help", "cc", "customcom"] {
            reserved.insert(name.to_owned());
        }
        reserved
    }

    /// A server's custom command, if `name` is one: the reply text, placeholders filled in.
    fn custom_reply(&self, message: &IncomingMessage, name: &str, args: &str) -> Option<String> {
        let (store, guild) = (self.custom.as_ref()?, message.guild_id.as_deref()?);
        let text = store
            .custom_command(message.platform, guild, name)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "couldn't read a custom command");
                None
            })?;
        Some(crate::custom::render(
            &text,
            &message.author.id,
            &message.author.name,
            &message.channel_id,
            args,
        ))
    }

    /// The prefix commands use in this server: its own (`setprefix`) or the bot's.
    fn prefix_for(&self, platform: Platform, guild: Option<&str>) -> String {
        let saved = match (&self.store, guild) {
            (Some(store), Some(g)) => store
                .guild_setting(platform, g, crate::server::PREFIX_KEY)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "couldn't read the server prefix");
                    None
                }),
            _ => None,
        };
        saved.unwrap_or_else(|| self.prefix.clone())
    }

    /// The command in a message: with this server's prefix, or `setprefix` with the bot's own
    /// prefix too, so a forgotten server prefix can always be changed back.
    fn parse_command<'a>(&self, prefix: &str, content: &'a str) -> Option<(String, &'a str)> {
        command::parse(prefix, content).or_else(|| {
            command::parse(&self.prefix, content)
                .filter(|(name, _)| prefix != self.prefix && name == "setprefix")
        })
    }

    pub fn has_owners(&self) -> bool {
        !self.owners.is_empty()
    }

    pub fn register(&mut self, command: impl Command + 'static) {
        let command: Arc<dyn Command> = Arc::new(command);
        let names = std::iter::once(command.name()).chain(command.aliases().iter().copied());
        for name in names.collect::<Vec<_>>() {
            if self.commands.insert(name, command.clone()).is_some() {
                tracing::warn!(command = name, "command registered twice; last one wins");
            }
        }
    }

    /// Adapters call this once connected, so features can post without a triggering message.
    pub fn register_chat(&self, chat: Arc<dyn ChatAdapter>) {
        let platform = chat.platform();
        self.chats
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(platform, chat);
    }

    /// Adapters call this once they know their invite link (`invite` lists them). A second call
    /// for the same platform replaces its link.
    pub fn register_invite(&self, platform: Platform, url: String) {
        let mut info = self.invites.write().unwrap_or_else(|p| p.into_inner());
        info.own.retain(|(p, _)| *p != platform);
        info.own.push((platform, url));
        info.own.sort_by_key(|(p, _)| p.to_string());
    }

    /// Which release channel this build is, its prefix before an experimental build doubles it,
    /// and the other channel's bot's invite links, for `invite`.
    pub fn set_release_channel(&self, experimental: bool, base_prefix: &str, others: Vec<String>) {
        let mut info = self.invites.write().unwrap_or_else(|p| p.into_inner());
        info.experimental = experimental;
        info.base_prefix = base_prefix.to_owned();
        info.others = others;
    }

    pub fn chat(&self, platform: Platform) -> Option<Arc<dyn ChatAdapter>> {
        self.chats
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&platform)
            .cloned()
    }

    async fn level_of(&self, chat: &dyn ChatAdapter, message: &IncomingMessage) -> Level {
        let is_owner = self.owners.contains(&message.author.id);
        match &self.permissions {
            Some(p) => {
                p.level(
                    chat,
                    message.guild_id.as_deref(),
                    &message.author.id,
                    is_owner,
                )
                .await
            }
            None if is_owner => Level::BotOwner,
            None => Level::Everyone,
        }
    }

    /// Entry point for adapters: call this for every message received.
    pub async fn handle_message(&self, chat: Arc<dyn ChatAdapter>, message: IncomingMessage) {
        if message.author.is_bot {
            return;
        }
        let prefix = self.prefix_for(message.platform, message.guild_id.as_deref());
        let Some((name, args)) = self.parse_command(&prefix, &message.content) else {
            return;
        };
        let args = args.to_owned();

        if name == "help" {
            let level = self.level_of(&*chat, &message).await;
            for text in self.help_messages(level, &prefix) {
                if let Err(e) = chat.send_message(&message.channel_id, &text).await {
                    tracing::warn!(error = %e, "failed to send help");
                    break;
                }
            }
            return;
        }

        let Some(cmd) = self.commands.get(name.as_str()).cloned() else {
            // Not built in: maybe this server's custom command. Otherwise ignored, like Red.
            if let Some(text) = self.custom_reply(&message, &name, &args) {
                if let Err(e) = chat.send_message(&message.channel_id, &text).await {
                    tracing::warn!(error = %e, command = %name, "couldn't post a custom command");
                }
            }
            return;
        };

        let channel_id = message.channel_id.clone();
        let platform = message.platform;
        let is_owner = self.owners.contains(&message.author.id);
        tracing::debug!(%platform, command = %name, user = %message.author.id, "running command");

        // Only look the level up (a few API calls) when the command needs it.
        let level = if cmd.level() > Level::Everyone || cmd.needs_level() {
            let level = self.level_of(&*chat, &message).await;
            if level < cmd.level() {
                let text = format!(
                    "You need to be {} to use `{}{}`.",
                    cmd.level(),
                    prefix,
                    name
                );
                let _ = chat.send_message(&channel_id, &text).await;
                return;
            }
            level
        } else if is_owner {
            Level::BotOwner
        } else {
            Level::Everyone
        };

        let ctx = CommandContext {
            chat: chat.clone(),
            is_owner,
            prefix: prefix.clone(),
            bot_prefix: prefix.clone(),
            level,
            message,
            args,
        };
        if let Err(err) = cmd.run(ctx).await {
            let shown = match &err {
                Error::Usage(_) | Error::Unsupported { .. } => err.to_string(),
                _ => {
                    tracing::error!(%platform, command = %name, error = %err, "command failed");
                    "Something went wrong running that command.".to_owned()
                }
            };
            let _ = chat.send_message(&channel_id, &shown).await;
        }
    }

    /// `help`: one message per section of [`HELP_SECTIONS`], with only what `level` may run.
    /// Groups list their subcommands indented under them. A section longer than one message
    /// is split at line breaks (if that starts happening, the layout needs revisiting).
    fn help_messages(&self, level: Level, prefix: &str) -> Vec<String> {
        let visible: Vec<&Arc<dyn Command>> = self
            .commands
            .iter()
            .filter(|(name, c)| **name == c.name() && c.level() <= level)
            .filter(|(name, _)| !HELP_HIDDEN.contains(name))
            .map(|(_, c)| c)
            .collect();
        let listed: Vec<&str> = HELP_SECTIONS
            .iter()
            .flat_map(|(_, names)| names.iter().copied())
            .collect();

        let mut messages = Vec::new();
        for (i, (title, names)) in HELP_SECTIONS.iter().enumerate() {
            let mut section: Vec<&Arc<dyn Command>> = names
                .iter()
                .filter_map(|n| visible.iter().find(|c| c.name() == *n).copied())
                .collect();
            // Commands nobody placed go in the last section, so they never disappear.
            if i == HELP_SECTIONS.len() - 1 {
                section.extend(visible.iter().filter(|c| !listed.contains(&c.name())));
            }
            if section.is_empty() {
                continue;
            }
            messages.extend(help_section(title, &section_lines(&section, level, prefix)));
        }
        messages
    }
}

/// How `help` groups commands, one message each, in order. Commands in no section and not
/// hidden go in the last one.
const HELP_SECTIONS: &[(&str, &[&str])] = &[
    ("Administration", &["admin", "mod", "set", "setprefix"]),
    (
        "Cool Things",
        &["music", "cc", "invite", "tube", "twitch", "feedback"],
    ),
];

/// Commands that work but aren't listed in `help`.
const HELP_HIDDEN: &[&str] = &["ping"];

/// Discord's limit is 2000 characters; leave room.
const HELP_MESSAGE_MAX: usize = 1900;

/// The lines of one section: `$name  help`, and each group's subcommands indented under it.
/// Names line up in a column. Backticks in help texts would show literally in a code block,
/// so they're dropped.
fn section_lines(section: &[&Arc<dyn Command>], level: Level, prefix: &str) -> Vec<String> {
    let width = section
        .iter()
        .map(|c| prefix.chars().count() + c.name().chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for cmd in section {
        let name = format!("{prefix}{}", cmd.name());
        lines.push(format!("{name:<width$}  {}", cmd.help().replace('`', "")));
        let subs = cmd.subcommands(level);
        let sub_width = subs
            .iter()
            .map(|(n, _)| n.chars().count())
            .max()
            .unwrap_or(0);
        for (sub, help) in subs {
            lines.push(format!("    {sub:<sub_width$}  {}", help.replace('`', "")));
        }
    }
    lines
}

/// `**title**` and the lines in a code block, as one message or, past the limit, several
/// (the title repeated with "continued").
fn help_section(title: &str, lines: &[String]) -> Vec<String> {
    let mut messages = Vec::new();
    let mut current = format!("**{title}**\n```\n");
    for line in lines {
        if current.len() + line.len() + 5 > HELP_MESSAGE_MAX && current.contains('\n') {
            current.push_str("```");
            messages.push(current);
            current = format!("**{title}** (continued)\n```\n");
        }
        current.push_str(line);
        current.push('\n');
    }
    current.push_str("```");
    messages.push(current);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_links_one_per_platform_sorted() {
        let bot = Bot::new("!", Vec::new());
        bot.register_invite(Platform::Fluxer, "https://f/old".into());
        bot.register_invite(Platform::Discord, "https://d".into());
        bot.register_invite(Platform::Fluxer, "https://f/new".into());
        let links = bot.invites.read().unwrap().own.clone();
        assert_eq!(
            links,
            vec![
                (Platform::Discord, "https://d".to_owned()),
                (Platform::Fluxer, "https://f/new".to_owned())
            ]
        );
        assert!(bot.commands.contains_key("invite"));
    }

    #[test]
    fn server_prefix_replaces_the_default_except_for_setprefix() {
        let mut bot = Bot::new("!", Vec::new());
        let store = Arc::new(crate::storage::Store::open_in_memory().unwrap());
        store
            .set_guild_setting(Platform::Fluxer, "g", crate::server::PREFIX_KEY, Some("?"))
            .unwrap();
        bot.enable_server_prefixes(store.clone());
        let p = bot.prefix_for(Platform::Fluxer, Some("g"));
        assert_eq!(p, "?");
        assert_eq!(
            bot.prefix_for(Platform::Discord, Some("g")),
            "!",
            "per platform"
        );
        assert_eq!(
            bot.prefix_for(Platform::Fluxer, None),
            "!",
            "DMs use the default"
        );
        assert_eq!(
            bot.parse_command(&p, "?ping").map(|c| c.0),
            Some("ping".into())
        );
        assert_eq!(bot.parse_command(&p, "!ping"), None);
        assert_eq!(
            bot.parse_command(&p, "!setprefix reset").map(|c| c.0),
            Some("setprefix".into()),
            "the default prefix still reaches setprefix"
        );
        // Written elsewhere (an import): applies to the next message.
        store
            .set_guild_setting(Platform::Fluxer, "g", crate::server::PREFIX_KEY, Some("."))
            .unwrap();
        assert_eq!(bot.prefix_for(Platform::Fluxer, Some("g")), ".");
    }

    #[test]
    fn custom_commands_answer_and_builtins_are_reserved() {
        let mut bot = Bot::new("!", Vec::new());
        let store = Arc::new(crate::storage::Store::open_in_memory().unwrap());
        bot.enable_custom_commands(store.clone());
        store
            .set_custom_command(Platform::Fluxer, "g", "hi", "Hi {author}, {0}!", "u")
            .unwrap();
        let message = |guild: Option<&str>| IncomingMessage {
            platform: Platform::Fluxer,
            guild_id: guild.map(str::to_owned),
            channel_id: "c".into(),
            message_id: "m".into(),
            author: crate::adapter::Author {
                id: "7".into(),
                name: "Seven".into(),
                is_bot: false,
            },
            content: String::new(),
            attachments: Vec::new(),
        };
        assert_eq!(
            bot.custom_reply(&message(Some("g")), "hi", "there"),
            Some("Hi <@7>, there!".into())
        );
        assert_eq!(bot.custom_reply(&message(Some("g")), "nope", ""), None);
        assert_eq!(
            bot.custom_reply(&message(None), "hi", ""),
            None,
            "not in DMs"
        );
        assert_eq!(
            bot.custom_reply(&message(Some("other")), "hi", ""),
            None,
            "per server"
        );
        // Built-ins and their aliases are reserved.
        let reserved = bot.reserved_names();
        for name in [
            "ping",
            "invite",
            "feedback",
            "bug",
            "help",
            "cc",
            "customcom",
        ] {
            assert!(reserved.contains(name), "{name} should be reserved");
        }
        assert!(!reserved.contains("hi"));
    }

    #[test]
    fn help_is_grouped_and_hides_what_you_cant_run() {
        let bot = Bot::new("!", Vec::new());
        // Built into Bot::new: invite and feedback (Cool Things), ping (hidden).
        let messages = bot.help_messages(Level::Everyone, "$");
        assert_eq!(
            messages.len(),
            1,
            "no Administration command registered: {messages:?}"
        );
        let cool = &messages[0];
        assert!(cool.starts_with("**Cool Things**\n```\n"), "{cool}");
        assert!(cool.ends_with("```"), "{cool}");
        assert!(cool.contains("\n$invite  "), "{cool}");
        assert!(!cool.contains("$ping"), "ping is hidden");
        assert!(!cool.contains("$help"), "help isn't listed");
    }

    #[test]
    fn long_sections_split_into_messages() {
        let lines: Vec<String> = (0..100)
            .map(|i| format!("$cmd{i:03}  {}", "x".repeat(40)))
            .collect();
        let messages = help_section("Big", &lines);
        assert!(messages.len() > 1);
        assert!(messages.iter().all(|m| m.chars().count() <= 2000));
        assert!(messages[1].starts_with("**Big** (continued)\n```\n"));
        assert!(messages.iter().all(|m| m.ends_with("```")));
        let total: usize = messages.iter().map(|m| m.matches("$cmd").count()).sum();
        assert_eq!(total, 100, "no line lost");
    }
}
