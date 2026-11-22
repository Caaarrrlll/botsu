//! Custom commands (`cc`), like Red's CustomCommands: a server's own `<prefix><name>` that replies
//! with a saved text. Mods create, edit and delete them; anyone lists, shows and uses them.
//!
//! The text may use `{author}` (a mention), `{author.name}`, `{channel}` (a link to the channel)
//! and `{0}` to `{9}` (the words typed after the command). Built-in commands always win, and a
//! custom command can't take a built-in's name.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;

use crate::alerts::escape_md;
use crate::command::{Command, CommandContext};
use crate::error::{Error, Result};
use crate::permissions::Level;
use crate::server::guild;
use crate::storage::Store;

const NAME_MAX: usize = 32;
const TEXT_MAX: usize = 1900;
const LIST_MAX: usize = 1900;

/// `[a-z0-9_-]`, 1 to 32 characters (names are matched in lower case, like built-ins).
pub fn valid_name(name: &str) -> bool {
    (1..=NAME_MAX).contains(&name.chars().count())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Fills in the placeholders. `args` is what was typed after the command name.
pub fn render(
    text: &str,
    author_id: &str,
    author_name: &str,
    channel_id: &str,
    args: &str,
) -> String {
    let words: Vec<&str> = args.split_whitespace().collect();
    let mut out = text
        .replace("{author.name}", &escape_md(author_name))
        .replace("{author}", &format!("<@{author_id}>"))
        .replace("{channel}", &format!("<#{channel_id}>"));
    for i in 0..10 {
        out = out.replace(&format!("{{{i}}}"), words.get(i).copied().unwrap_or(""));
    }
    out
}

/// `cc create|edit|delete|list|show`.
pub struct CustomCommand {
    store: Arc<Store>,
    /// Built-in command names and aliases: custom commands can't take them.
    reserved: BTreeSet<String>,
}

impl CustomCommand {
    pub fn new(store: Arc<Store>, reserved: BTreeSet<String>) -> Self {
        Self { store, reserved }
    }

    fn usage(p: &str) -> String {
        format!(
            "Custom commands:\n\
             `{p}cc create <name> <text>`: add one (mods)\n\
             `{p}cc edit <name> <text>`, `{p}cc delete <name>` (mods)\n\
             `{p}cc list`, `{p}cc show <name>`\n\
             In the text: `{{author}}` (a mention), `{{author.name}}`, `{{channel}}`, and \
             `{{0}}` to `{{9}}` for the words typed after the command."
        )
    }

    fn require_mod(ctx: &CommandContext) -> Result<()> {
        if ctx.level >= Level::Mod {
            Ok(())
        } else {
            Err(Error::Usage(format!(
                "You need to be {} to do that.",
                Level::Mod
            )))
        }
    }
}

/// `"<name> <text…>"`: the name lower-cased, the text with its line breaks kept.
fn split_name(rest: &str) -> (String, &str) {
    let rest = rest.trim_start();
    match rest.split_once(char::is_whitespace) {
        Some((name, text)) => (name.to_lowercase(), text.trim()),
        None => (rest.to_lowercase(), ""),
    }
}

#[async_trait]
impl Command for CustomCommand {
    fn name(&self) -> &'static str {
        "cc"
    }

    fn aliases(&self) -> &[&'static str] {
        &["customcom"]
    }

    fn help(&self) -> &'static str {
        "This server's custom commands: create, edit, delete (mods), list, show."
    }

    // create/edit/delete need mod; list/show are for everyone.
    fn needs_level(&self) -> bool {
        true
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let platform = ctx.message.platform;
        let args = ctx.args.trim_start();
        let (sub, rest) = match args.split_once(char::is_whitespace) {
            Some((s, r)) => (s.to_lowercase(), r),
            None => (args.to_lowercase(), ""),
        };
        let p = ctx.prefix.clone();

        let reply = match sub.as_str() {
            "create" | "add" | "edit" => {
                Self::require_mod(&ctx)?;
                let (name, text) = split_name(rest);
                if !valid_name(&name) || text.is_empty() {
                    return Err(Error::Usage(Self::usage(&p)));
                }
                if self.reserved.contains(&name) {
                    return Err(Error::Usage(format!(
                        "`{name}` is one of my own commands; pick another name."
                    )));
                }
                if text.chars().count() > TEXT_MAX {
                    return Err(Error::Usage(format!(
                        "Keep the text under {TEXT_MAX} characters."
                    )));
                }
                let exists = self
                    .store
                    .custom_command(platform, &guild, &name)?
                    .is_some();
                match (sub.as_str(), exists) {
                    ("edit", false) => {
                        return Err(Error::Usage(format!(
                            "There's no `{p}{name}` here. `{p}cc create {name} <text>` adds it."
                        )));
                    }
                    ("create" | "add", true) => {
                        return Err(Error::Usage(format!(
                            "`{p}{name}` already exists. `{p}cc edit {name} <text>` changes it."
                        )));
                    }
                    _ => {}
                }
                self.store.set_custom_command(
                    platform,
                    &guild,
                    &name,
                    text,
                    &ctx.message.author.id,
                )?;
                if exists {
                    format!("Changed `{p}{name}`.")
                } else {
                    format!("Added `{p}{name}`.")
                }
            }
            "delete" | "remove" | "del" => {
                Self::require_mod(&ctx)?;
                let (name, _) = split_name(rest);
                if self.store.delete_custom_command(platform, &guild, &name)? {
                    format!("Deleted `{p}{name}`.")
                } else {
                    return Err(Error::Usage(format!("There's no `{p}{name}` here.")));
                }
            }
            "show" => {
                let (name, _) = split_name(rest);
                match self.store.custom_command(platform, &guild, &name)? {
                    // In a code block, so the placeholders and mentions show as typed.
                    Some(text) => format!("`{p}{name}`:\n```\n{}\n```", text.replace("```", "'''")),
                    None => return Err(Error::Usage(format!("There's no `{p}{name}` here."))),
                }
            }
            "list" => {
                let names: Vec<String> = self
                    .store
                    .custom_commands(platform, &guild)?
                    .into_iter()
                    .map(|(name, _)| format!("`{p}{name}`"))
                    .collect();
                if names.is_empty() {
                    format!("No custom commands here yet. `{p}cc create <name> <text>` adds one.")
                } else {
                    let mut out = format!("Custom commands ({}): ", names.len());
                    for (i, n) in names.iter().enumerate() {
                        if out.len() + n.len() + 2 > LIST_MAX {
                            out.push_str(&format!("… and {} more", names.len() - i));
                            break;
                        }
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(n);
                    }
                    out
                }
            }
            _ => Self::usage(&p),
        };
        ctx.reply(&reply).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("rules"));
        assert!(valid_name("server-ip_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("Rules"));
        assert!(!valid_name("two words"));
        assert!(!valid_name(&"x".repeat(33)));
    }

    #[test]
    fn placeholders() {
        assert_eq!(
            render(
                "Hi {author} ({author.name}), see {channel}. {0}+{1}={2}",
                "7",
                "a_b",
                "9",
                "1 2"
            ),
            "Hi <@7> (a\\_b), see <#9>. 1+2="
        );
        assert_eq!(render("plain", "7", "x", "9", ""), "plain");
    }

    #[test]
    fn name_and_text() {
        assert_eq!(
            split_name(" Rules  Be nice.\nNo spam."),
            ("rules".into(), "Be nice.\nNo spam.")
        );
        assert_eq!(split_name("rules"), ("rules".into(), ""));
    }
}
