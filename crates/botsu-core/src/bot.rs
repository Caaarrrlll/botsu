use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::adapter::{ChatAdapter, IncomingMessage, Platform};
use crate::command::{self, Command, CommandContext, Invite, InviteLinks, Ping};
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
        };
        bot.register(Ping);
        bot.register(Invite(bot.invites.clone()));
        bot
    }

    pub fn set_permissions(&mut self, permissions: Arc<Permissions>) {
        self.permissions = Some(permissions);
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
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
        let mut links = self.invites.write().unwrap_or_else(|p| p.into_inner());
        links.retain(|(p, _)| *p != platform);
        links.push((platform, url));
        links.sort_by_key(|(p, _)| p.to_string());
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
        let Some((name, args)) = command::parse(&self.prefix, &message.content) else {
            return;
        };
        let args = args.to_owned();

        if name == "help" {
            let level = self.level_of(&*chat, &message).await;
            let text = self.help_text(level);
            if let Err(e) = chat.send_message(&message.channel_id, &text).await {
                tracing::warn!(error = %e, "failed to send help");
            }
            return;
        }

        let Some(cmd) = self.commands.get(name.as_str()).cloned() else {
            return; // Unknown commands are ignored silently, like Red.
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
                    self.prefix,
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
            prefix: self.prefix.clone(),
            bot_prefix: self.prefix.clone(),
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

    /// Only the commands `level` may run.
    fn help_text(&self, level: Level) -> String {
        let mut out = String::from("Commands:\n");
        out.push_str(&format!("`{}help`: show this list\n", self.prefix));
        for (_, cmd) in self
            .commands
            .iter()
            .filter(|(name, c)| **name == c.name() && c.level() <= level)
        {
            out.push_str(&format!(
                "`{}{}`: {}\n",
                self.prefix,
                cmd.name(),
                cmd.help()
            ));
        }
        out
    }
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
        let links = bot.invites.read().unwrap().clone();
        assert_eq!(
            links,
            vec![
                (Platform::Discord, "https://d".to_owned()),
                (Platform::Fluxer, "https://f/new".to_owned())
            ]
        );
        assert!(bot.commands.contains_key("invite"));
    }
}
