use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::adapter::{ChatAdapter, IncomingMessage, Platform};
use crate::command::{self, Command, CommandContext, Ping};
use crate::error::Error;

/// The shared, platform-neutral bot. One instance serves every adapter.
pub struct Bot {
    prefix: String,
    /// User IDs (any platform) allowed to run admin commands. Phase 4 replaces this with roles.
    owners: HashSet<String>,
    commands: BTreeMap<&'static str, Arc<dyn Command>>,
    /// Connected platforms, for sending outside a command (alerts).
    chats: RwLock<HashMap<Platform, Arc<dyn ChatAdapter>>>,
}

impl Bot {
    pub fn new(prefix: impl Into<String>, owners: impl IntoIterator<Item = String>) -> Self {
        let mut bot = Self {
            prefix: prefix.into(),
            owners: owners.into_iter().collect(),
            commands: BTreeMap::new(),
            chats: RwLock::default(),
        };
        bot.register(Ping);
        bot
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn has_owners(&self) -> bool {
        !self.owners.is_empty()
    }

    pub fn register(&mut self, command: impl Command + 'static) {
        let name = command.name();
        if self.commands.insert(name, Arc::new(command)).is_some() {
            tracing::warn!(command = name, "command registered twice; last one wins");
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

    pub fn chat(&self, platform: Platform) -> Option<Arc<dyn ChatAdapter>> {
        self.chats
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&platform)
            .cloned()
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
            let text = self.help_text();
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
        tracing::debug!(%platform, command = %name, user = %message.author.id, "running command");

        let ctx = CommandContext {
            chat: chat.clone(),
            is_owner: self.owners.contains(&message.author.id),
            prefix: self.prefix.clone(),
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

    fn help_text(&self) -> String {
        let mut out = String::from("Commands:\n");
        out.push_str(&format!("`{}help`: show this list\n", self.prefix));
        for cmd in self.commands.values() {
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
