use std::collections::BTreeMap;
use std::sync::Arc;

use crate::adapter::{ChatAdapter, IncomingMessage};
use crate::command::{self, Command, CommandContext, Ping};
use crate::error::Error;

/// The shared, platform-neutral bot. One instance serves every adapter.
pub struct Bot {
    prefix: String,
    commands: BTreeMap<&'static str, Arc<dyn Command>>,
}

impl Bot {
    pub fn new(prefix: impl Into<String>) -> Self {
        let mut bot = Self {
            prefix: prefix.into(),
            commands: BTreeMap::new(),
        };
        bot.register(Ping);
        bot
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn register(&mut self, command: impl Command + 'static) {
        let name = command.name();
        if self.commands.insert(name, Arc::new(command)).is_some() {
            tracing::warn!(command = name, "command registered twice; last one wins");
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
