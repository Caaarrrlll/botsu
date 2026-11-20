//! Server tools (phase 4): permission setup, cleanup, roles and announcements.
//!
//! | Command | Level |
//! |---|---|
//! | `set adminrole/modrole add/remove <role>`, `set roles`, `set modlog`, `set muterole`, `set moddm` | server owner |
//! | `cleanup messages/user/bots` | mod |
//! | `addrole`, `removerole`, `announce` | admin |

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::adapter::{ChatAdapter, RoleInfo, ADMINISTRATOR};
use crate::alerts::parse_channel;
use crate::command::{Command, CommandContext, Group};
use crate::error::{Error, Result};
use crate::moderation::{MODLOG_KEY, MOD_DM_KEY, MUTE_ROLE_KEY};
use crate::permissions::{Level, ADMIN_ROLE, MOD_ROLE};
use crate::storage::{now, Store};

/// `<@123>`, `<@!123>` or `123` → `123`.
pub fn parse_user(arg: &str) -> Option<String> {
    let id = arg
        .strip_prefix("<@")
        .and_then(|s| s.strip_suffix('>'))
        .map(|s| s.trim_start_matches('!'))
        .unwrap_or(arg);
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_owned())
}

pub(crate) fn guild(ctx: &CommandContext) -> Result<String> {
    ctx.message
        .guild_id
        .clone()
        .ok_or_else(|| Error::Usage("This only works in a server.".into()))
}

/// A role by mention (`<@&id>`), ID, or name (case-insensitive).
async fn resolve_role(ctx: &CommandContext, guild: &str, arg: &str) -> Result<RoleInfo> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err(Error::Usage(
            "Which role? Mention it, or give its name or ID.".into(),
        ));
    }
    let roles = ctx.chat.roles(guild).await?;
    let id = arg
        .strip_prefix("<@&")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(arg);
    roles
        .iter()
        .find(|r| r.id == id)
        .or_else(|| roles.iter().find(|r| r.name.eq_ignore_ascii_case(arg)))
        .cloned()
        .ok_or_else(|| Error::Usage(format!("There's no role called `{arg}` here.")))
}

// --- set ------------------------------------------------------------------------------------------

/// `set adminrole|modrole add|remove <role>` and `set roles`.
pub struct SetCommand {
    store: Arc<Store>,
}

impl SetCommand {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    fn usage(p: &str) -> String {
        format!(
            "Permissions (server owner):\n\
             `{p}set adminrole add <role>` / `{p}set adminrole remove <role>`\n\
             `{p}set modrole add <role>` / `{p}set modrole remove <role>`\n\
             `{p}set roles`: show who counts as admin and mod here\n\
             `{p}set modlog <#channel>` / `{p}set modlog off`: where moderation cases are posted
             `{p}set muterole <role>` / `{p}set muterole off`: the role `mute` gives (set its              channel permissions yourself)
             `{p}set moddm on|off`: DM members when they're warned, kicked, banned, timed out or              muted (off by default)"
        )
    }

    async fn muterole(&self, ctx: &CommandContext, guild: &str, arg: &str) -> Result<()> {
        let platform = ctx.message.platform;
        let reply = if arg.is_empty() {
            match self.store.guild_setting(platform, guild, MUTE_ROLE_KEY)? {
                Some(r) => format!("The mute role is <@&{r}>."),
                None => format!(
                    "No mute role is set, so `{p}mute` is a timeout. `{p}set muterole <role>`                      sets one.",
                    p = ctx.prefix
                ),
            }
        } else if arg.eq_ignore_ascii_case("off") {
            self.store
                .set_guild_setting(platform, guild, MUTE_ROLE_KEY, None)?;
            "No mute role any more: `mute` is a timeout again. Members who have the role keep it."
                .to_owned()
        } else {
            let role = resolve_role(ctx, guild, arg).await?;
            if role.id == guild {
                return Err(Error::Usage("@everyone can't be the mute role.".into()));
            }
            if role.permissions & ADMINISTRATOR != 0 {
                return Err(Error::Usage(format!(
                    "**{}** has Administrator; pick a role without it.",
                    role.name
                )));
            }
            self.store
                .set_guild_setting(platform, guild, MUTE_ROLE_KEY, Some(&role.id))?;
            format!(
                "`mute` now gives **{}**. Make sure it denies Send Messages (and Speak) in the                  channels, and that my highest role is above it.",
                role.name
            )
        };
        ctx.reply(&reply).await?;
        Ok(())
    }

    async fn moddm(&self, ctx: &CommandContext, guild: &str, arg: &str) -> Result<()> {
        let platform = ctx.message.platform;
        let reply = match arg.to_lowercase().as_str() {
            "on" => {
                self.store
                    .set_guild_setting(platform, guild, MOD_DM_KEY, Some("on"))?;
                "Members will get a DM saying what happened and why. (Some have DMs closed; the                  action still goes ahead.)"
            }
            "off" => {
                self.store
                    .set_guild_setting(platform, guild, MOD_DM_KEY, None)?;
                "Members won't get a DM about moderation actions."
            }
            "" => {
                if self
                    .store
                    .guild_setting(platform, guild, MOD_DM_KEY)?
                    .as_deref()
                    == Some("on")
                {
                    "Moderation DMs are on."
                } else {
                    "Moderation DMs are off."
                }
            }
            _ => return Err(Error::Usage(Self::usage(&ctx.prefix))),
        };
        ctx.reply(reply).await?;
        Ok(())
    }

    async fn modlog(&self, ctx: &CommandContext, guild: &str, arg: &str) -> Result<()> {
        let platform = ctx.message.platform;
        let arg = arg.trim();
        let reply = if arg.is_empty() {
            match self.store.guild_setting(platform, guild, MODLOG_KEY)? {
                Some(c) => format!("Moderation cases are posted in <#{c}>."),
                None => format!(
                    "No mod-log channel is set. `{}set modlog <#channel>` sets one.",
                    ctx.prefix
                ),
            }
        } else if arg.eq_ignore_ascii_case("off") {
            self.store
                .set_guild_setting(platform, guild, MODLOG_KEY, None)?;
            "Moderation cases are no longer posted anywhere (they're still recorded).".to_owned()
        } else {
            let channel =
                parse_channel(arg).ok_or_else(|| Error::Usage(Self::usage(&ctx.prefix)))?;
            ctx.ensure_channel_here(&channel).await?;
            self.store
                .set_guild_setting(platform, guild, MODLOG_KEY, Some(&channel))?;
            format!("Moderation cases will be posted in <#{channel}>.")
        };
        ctx.reply(&reply).await?;
        Ok(())
    }
}

#[async_trait]
impl Command for SetCommand {
    fn name(&self) -> &'static str {
        "set"
    }

    fn help(&self) -> &'static str {
        "Choose admin and mod roles, the mod-log channel, the mute role and mod DMs (server owner)."
    }

    fn level(&self) -> Level {
        Level::ServerOwner
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let platform = ctx.message.platform;
        let mut words = ctx.args.splitn(3, char::is_whitespace);
        let what = words.next().unwrap_or("").to_lowercase();
        let action = words.next().unwrap_or("").to_lowercase();
        let rest = words.next().unwrap_or("").trim().to_owned();

        if what == "modlog" {
            let arg = ctx.args.trim_start()["modlog".len()..].to_owned();
            return self.modlog(&ctx, &guild, &arg).await;
        }
        if what == "muterole" || what == "moddm" {
            let arg = ctx.args.trim_start()[what.len()..].trim().to_owned();
            return if what == "muterole" {
                self.muterole(&ctx, &guild, &arg).await
            } else {
                self.moddm(&ctx, &guild, &arg).await
            };
        }
        if what == "roles" {
            let configured = self.store.guild_roles(platform, &guild)?;
            let list = |level: &str| {
                let ids: Vec<String> = configured
                    .iter()
                    .filter(|(l, _)| l == level)
                    .map(|(_, r)| format!("<@&{r}>"))
                    .collect();
                if ids.is_empty() {
                    "none set".to_owned()
                } else {
                    ids.join(", ")
                }
            };
            ctx.reply(&format!(
                "**Admin roles:** {}\n**Mod roles:** {}\nThe server owner and anyone with the \
                 Administrator permission always count as admin; admins can do everything mods \
                 can.",
                list(ADMIN_ROLE),
                list(MOD_ROLE)
            ))
            .await?;
            return Ok(());
        }

        let level = match what.as_str() {
            "adminrole" => ADMIN_ROLE,
            "modrole" => MOD_ROLE,
            _ => return Err(Error::Usage(Self::usage(&ctx.prefix))),
        };
        let role = resolve_role(&ctx, &guild, &rest).await?;
        let reply = match action.as_str() {
            "add" => {
                if self
                    .store
                    .add_guild_role(platform, &guild, level, &role.id)?
                {
                    format!("**{}** now counts as {level}.", role.name)
                } else {
                    format!("**{}** already counts as {level}.", role.name)
                }
            }
            "remove" => {
                if self
                    .store
                    .remove_guild_role(platform, &guild, level, &role.id)?
                {
                    format!("**{}** no longer counts as {level}.", role.name)
                } else {
                    format!("**{}** wasn't a {level} role.", role.name)
                }
            }
            _ => return Err(Error::Usage(Self::usage(&ctx.prefix))),
        };
        ctx.reply(&reply).await?;
        Ok(())
    }
}

// --- cleanup --------------------------------------------------------------------------------------

/// Most messages one cleanup deletes, and most it looks through to find them.
const CLEANUP_MAX: usize = 1000;
const CLEANUP_SCAN_MAX: usize = 1000;
/// Discord only bulk-deletes messages younger than 14 days; applied to every platform.
const BULK_MAX_AGE_SECS: i64 = 14 * 24 * 3600 - 300;

enum Filter {
    All,
    User(String),
    /// Bot messages and messages starting with the prefix (bot commands).
    Bots(String),
}

/// `cleanup messages <n>`, `cleanup user <user> <n>`, `cleanup bots <n>`.
pub struct CleanupCommand;

impl CleanupCommand {
    fn usage(p: &str) -> String {
        format!(
            "Cleanup (mods):\n\
             `{p}cleanup messages <n>`: the last n messages\n\
             `{p}cleanup user <@user> <n>`: that user's last n messages\n\
             `{p}cleanup bots <n>`: the last n bot messages and bot commands\n\
             Up to {CLEANUP_MAX}; messages older than 14 days can't be bulk-deleted."
        )
    }
}

fn parse_count(arg: Option<&str>, p: &str) -> Result<usize> {
    arg.and_then(|a| a.parse::<usize>().ok())
        .filter(|n| (1..=CLEANUP_MAX).contains(n))
        .ok_or_else(|| Error::Usage(CleanupCommand::usage(p)))
}

/// Deletes `ids` in chunks of 100, falling back to one by one when a bulk delete is refused.
/// Returns how many were deleted.
async fn delete_messages(chat: &dyn ChatAdapter, channel: &str, ids: &[String]) -> usize {
    let mut deleted = 0;
    for chunk in ids.chunks(100) {
        if chunk.len() >= 2 && chat.bulk_delete(channel, chunk).await.is_ok() {
            deleted += chunk.len();
            continue;
        }
        for id in chunk {
            match chat.delete_message(channel, id).await {
                Ok(()) => deleted += 1,
                Err(e) => tracing::info!(error = %e, message = %id, "couldn't delete message"),
            }
        }
    }
    deleted
}

#[async_trait]
impl Command for CleanupCommand {
    fn name(&self) -> &'static str {
        "cleanup"
    }

    fn help(&self) -> &'static str {
        "Bulk-delete recent messages: messages, user, bots (mods)."
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        guild(&ctx)?;
        let p = ctx.prefix.clone();
        let words: Vec<&str> = ctx.args.split_whitespace().collect();
        let (filter, count) = match words.as_slice() {
            ["messages" | "msgs", n] => (Filter::All, parse_count(Some(n), &p)?),
            ["user", who, n] => {
                let user = parse_user(who)
                    .ok_or_else(|| Error::Usage(format!("`{who}` isn't a user mention or ID.")))?;
                (Filter::User(user), parse_count(Some(n), &p)?)
            }
            ["bots" | "bot", n] => (
                Filter::Bots(ctx.bot_prefix.clone()),
                parse_count(Some(n), &p)?,
            ),
            _ => return Err(Error::Usage(Self::usage(&p))),
        };

        let channel = ctx.message.channel_id.clone();
        let cutoff = now() - BULK_MAX_AGE_SECS;
        let mut found: Vec<String> = Vec::new();
        let mut before = Some(ctx.message.message_id.clone());
        let (mut scanned, mut hit_old) = (0, false);
        'scan: while found.len() < count && scanned < CLEANUP_SCAN_MAX {
            let page = ctx
                .chat
                .recent_messages(&channel, before.as_deref(), 100)
                .await?;
            if page.is_empty() {
                break;
            }
            for m in page {
                scanned += 1;
                before = Some(m.id.clone());
                if m.timestamp > 0 && m.timestamp < cutoff {
                    hit_old = true;
                    break 'scan;
                }
                let wanted = match &filter {
                    Filter::All => true,
                    Filter::User(u) => &m.author_id == u,
                    Filter::Bots(prefix) => {
                        m.author_is_bot || m.content.starts_with(prefix.as_str())
                    }
                };
                if wanted {
                    found.push(m.id);
                    if found.len() == count {
                        break 'scan;
                    }
                }
            }
        }

        // The command message goes too, but isn't counted.
        let mut ids = found.clone();
        ids.push(ctx.message.message_id.clone());
        let deleted = delete_messages(&*ctx.chat, &channel, &ids)
            .await
            .min(found.len());

        let mut text = format!("Deleted {deleted} message(s).");
        if hit_old && deleted < count {
            text.push_str(" Stopped at messages older than 14 days, which can't be bulk-deleted.");
        }
        if deleted < found.len() {
            text.push_str(" Some couldn't be deleted (missing Manage Messages?).");
        }
        // The confirmation removes itself after a few seconds.
        let chat = ctx.chat.clone();
        if let Ok(id) = chat.send_message(&channel, &text).await {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let _ = chat.delete_message(&channel, &id).await;
            });
        }
        Ok(())
    }
}

/// `admin addrole | removerole | announce`.
pub fn admin_group() -> Group {
    Group::new(
        "admin",
        "Admin",
        "Admin tools: addrole, removerole, announce (admins).",
    )
    .with(RoleCommand::add())
    .with(RoleCommand::remove())
    .with(AnnounceCommand)
}

// --- roles ----------------------------------------------------------------------------------------

/// `addrole <user> <role>` / `removerole <user> <role>`.
pub struct RoleCommand {
    add: bool,
}

impl RoleCommand {
    pub fn add() -> Self {
        Self { add: true }
    }

    pub fn remove() -> Self {
        Self { add: false }
    }
}

#[async_trait]
impl Command for RoleCommand {
    fn name(&self) -> &'static str {
        if self.add {
            "addrole"
        } else {
            "removerole"
        }
    }

    fn help(&self) -> &'static str {
        if self.add {
            "Give a member a role (admins)."
        } else {
            "Take a role from a member (admins)."
        }
    }

    fn level(&self) -> Level {
        Level::Admin
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let (who, role_arg) = ctx
            .args
            .split_once(char::is_whitespace)
            .unwrap_or((ctx.args.as_str(), ""));
        let user = parse_user(who).ok_or_else(|| {
            Error::Usage(format!(
                "Usage: `{}{} <@user> <role>`",
                ctx.prefix,
                self.name()
            ))
        })?;
        let role = resolve_role(&ctx, &guild, role_arg).await?;
        // Roles carrying Administrator: only the server owner (or a bot owner) may hand them out.
        if self.add && role.permissions & ADMINISTRATOR != 0 && ctx.level < Level::ServerOwner {
            return Err(Error::Usage(format!(
                "**{}** has the Administrator permission; only the server owner can give it out.",
                role.name
            )));
        }
        let result = if self.add {
            ctx.chat.add_role(&guild, &user, &role.id).await
        } else {
            ctx.chat.remove_role(&guild, &user, &role.id).await
        };
        result.map_err(|e| {
            tracing::info!(error = %e, role = %role.name, "role change refused");
            Error::Usage(format!(
                "I couldn't change **{}**. I need Manage Roles, and my own role must be above it.",
                role.name
            ))
        })?;
        let done = if self.add { "Added" } else { "Removed" };
        ctx.reply(&format!("{done} the **{}** role.", role.name))
            .await?;
        Ok(())
    }
}

// --- announce -------------------------------------------------------------------------------------

/// `announce <#channel> <text>`: posts the text in that channel of this server.
pub struct AnnounceCommand;

#[async_trait]
impl Command for AnnounceCommand {
    fn name(&self) -> &'static str {
        "announce"
    }

    fn help(&self) -> &'static str {
        "Post a message in a channel (admins)."
    }

    fn level(&self) -> Level {
        Level::Admin
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        guild(&ctx)?;
        let usage = || Error::Usage(format!("Usage: `{}announce <#channel> <text>`", ctx.prefix));
        let (target, text) = ctx.args.split_once(char::is_whitespace).ok_or_else(usage)?;
        let channel = parse_channel(target).ok_or_else(usage)?;
        let text = text.trim();
        if text.is_empty() {
            return Err(usage());
        }
        ctx.ensure_channel_here(&channel).await?;
        ctx.chat.send_message(&channel, text).await.map_err(|e| {
            tracing::info!(error = %e, channel = %channel, "announce failed");
            Error::Usage(format!("I can't post in <#{channel}>."))
        })?;
        if channel != ctx.message.channel_id {
            ctx.reply(&format!("Posted in <#{channel}>.")).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_user;

    #[test]
    fn user_arguments() {
        assert_eq!(parse_user("<@123>").as_deref(), Some("123"));
        assert_eq!(parse_user("<@!123>").as_deref(), Some("123"));
        assert_eq!(parse_user("123").as_deref(), Some("123"));
        assert_eq!(parse_user("<@&123>"), None);
        assert_eq!(parse_user("bob"), None);
    }
}
