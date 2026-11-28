//! Server tools (phase 4): permission setup, cleanup, roles and announcements.
//!
//! | Command | Level |
//! |---|---|
//! | `set adminrole/modrole add/remove <role>`, `set roles`, `set modlog`, `set muterole`, `set moddm`, `set autorole` | server owner |
//! | `cleanup messages/user/bots` | mod |
//! | `addrole <role> <@user…>`, `removerole <role> <@user>`, `announce` | admin |

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::adapter::{ChatAdapter, RoleInfo, ADMINISTRATOR, MANAGE_ROLES};
use crate::alerts::{escape_md, parse_channel, EMBEDS_KEY};
use crate::command::{usage_block, Command, CommandContext, Group};
use crate::error::{Error, Result};
use crate::moderation::{MODLOG_KEY, MOD_DM_KEY, MUTE_ROLE_KEY};
use crate::permissions::{Level, ADMIN_ROLE, MOD_ROLE};
use crate::storage::{now, Store};

/// `guild_settings` key: the role every new member gets when they join (`set autorole`).
pub const AUTOROLE_KEY: &str = "autorole";

/// Permissions an autorole may not have: every newcomer, including raiders, would get them.
/// Kick, Ban, Administrator, Manage Channels, Manage Server, Manage Messages, Mention Everyone,
/// Manage Roles, Manage Webhooks, Moderate Members (same bits on Discord and Fluxer).
const STAFF_PERMISSIONS: u64 = (1 << 1)
    | (1 << 2)
    | ADMINISTRATOR
    | (1 << 4)
    | (1 << 5)
    | (1 << 13)
    | (1 << 17)
    | MANAGE_ROLES
    | (1 << 29)
    | (1 << 40);

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

// --- setprefix ------------------------------------------------------------------------------------

/// `guild_settings` key for a server's own command prefix.
pub const PREFIX_KEY: &str = "prefix";
const PREFIX_MAX: usize = 5;

/// `setprefix [new | reset]`: this server's command prefix (mods). The server's prefix replaces
/// the bot's default there; `setprefix` also answers to the default, so it can't be lost.
pub struct SetPrefixCommand {
    store: Arc<Store>,
    default: String,
}

impl SetPrefixCommand {
    pub(crate) fn new(store: Arc<Store>, default: String) -> Self {
        Self { store, default }
    }
}

/// A usable prefix: 1 to 5 characters, no spaces, and no backticks (they'd break the usage
/// hints, which are in code spans).
fn valid_prefix(p: &str) -> bool {
    let n = p.chars().count();
    (1..=PREFIX_MAX).contains(&n) && !p.chars().any(|c| c.is_whitespace() || c == '`')
}

#[async_trait]
impl Command for SetPrefixCommand {
    fn name(&self) -> &'static str {
        "setprefix"
    }

    fn help(&self) -> &'static str {
        "Show or change this server's command prefix; `reset` goes back to the default (mods)."
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let platform = ctx.message.platform;
        let current = self.store.guild_setting(platform, &guild, PREFIX_KEY)?;
        let arg = ctx.args.trim();
        let d = &self.default;

        let reply = if arg.is_empty() {
            match current {
                Some(p) => format!(
                    "This server's prefix is `{p}` (the default is `{d}`; `{d}setprefix` always \
                     works too)."
                ),
                None => format!("This server uses the default prefix, `{d}`."),
            }
        } else if arg.eq_ignore_ascii_case("reset") {
            self.store
                .set_guild_setting(platform, &guild, PREFIX_KEY, None)?;
            format!("Back to the default prefix, `{d}`.")
        } else if !valid_prefix(arg) {
            return Err(Error::Usage(format!(
                "A prefix is 1 to {PREFIX_MAX} characters, without spaces or backticks. \
                 Usage: `{}setprefix <prefix>`, `{}setprefix reset`.",
                ctx.prefix, ctx.prefix
            )));
        } else {
            let value = (arg != d.as_str()).then_some(arg);
            self.store
                .set_guild_setting(platform, &guild, PREFIX_KEY, value)?;
            format!(
                "Commands here now start with `{arg}`, for example `{arg}help`. If it's ever \
                 forgotten, `{d}setprefix` still works."
            )
        };
        ctx.reply(&reply).await?;
        Ok(())
    }
}

// --- set ------------------------------------------------------------------------------------------

/// `set` options, for `help` and the `set` usage message.
const SET_OPTIONS: &[(&str, &str)] = &[
    (
        "adminrole",
        "add|remove <role>: a role that counts as admin.",
    ),
    ("modrole", "add|remove <role>: a role that counts as mod."),
    ("roles", "Show who counts as admin and mod here."),
    (
        "modlog",
        "<#channel> | off: where moderation cases are posted.",
    ),
    (
        "muterole",
        "<role> | off: the role mute gives (set its channel permissions yourself).",
    ),
    (
        "moddm",
        "on | off: DM members about warns, kicks, bans, timeouts and mutes (off by default).",
    ),
    (
        "embeds",
        "on | off: alerts as an embed with a large image (on, default) or plain text.",
    ),
    (
        "autorole",
        "<role> | off: the role new members get when they join.",
    ),
];

/// `set adminrole|modrole add|remove <role>` and `set roles`.
pub struct SetCommand {
    store: Arc<Store>,
}

impl SetCommand {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    fn usage(p: &str) -> String {
        usage_block(
            "Server settings (server owner)",
            &format!("{p}set"),
            SET_OPTIONS,
        )
    }

    async fn muterole(&self, ctx: &CommandContext, guild: &str, arg: &str) -> Result<()> {
        let platform = ctx.message.platform;
        let reply = if arg.is_empty() {
            match self.store.guild_setting(platform, guild, MUTE_ROLE_KEY)? {
                Some(r) => format!("The mute role is <@&{r}>."),
                None => format!(
                    "No mute role is set, so `{p}mute` is a timeout. `{p}set muterole <role>` sets one.",
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
                "`mute` now gives **{}**. Make sure it denies Send Messages (and Speak) in the channels, and that my highest role is above it.",
                role.name
            )
        };
        ctx.reply(&reply).await?;
        Ok(())
    }

    async fn autorole(&self, ctx: &CommandContext, guild: &str, arg: &str) -> Result<()> {
        let platform = ctx.message.platform;
        let reply = if arg.is_empty() {
            match self.store.guild_setting(platform, guild, AUTOROLE_KEY)? {
                Some(r) => format!("New members get <@&{r}> when they join."),
                None => format!(
                    "New members get no role. `{}set autorole <role>` sets one.",
                    ctx.prefix
                ),
            }
        } else if arg.eq_ignore_ascii_case("off") {
            self.store
                .set_guild_setting(platform, guild, AUTOROLE_KEY, None)?;
            "New members won't get a role any more. Members who have it keep it.".to_owned()
        } else {
            let role = resolve_role(ctx, guild, arg).await?;
            if role.id == guild {
                return Err(Error::Usage("Everyone has @everyone already.".into()));
            }
            if role.permissions & STAFF_PERMISSIONS != 0 {
                return Err(Error::Usage(format!(
                    "**{}** has moderator or admin permissions, and every newcomer would get                      them. Pick a role without them.",
                    role.name
                )));
            }
            self.store
                .set_guild_setting(platform, guild, AUTOROLE_KEY, Some(&role.id))?;
            format!(
                "New members will get **{}** when they join. I need Manage Roles, and my highest                  role must be above it.",
                role.name
            )
        };
        ctx.reply(&reply).await?;
        Ok(())
    }

    async fn embeds(&self, ctx: &CommandContext, guild: &str, arg: &str) -> Result<()> {
        let platform = ctx.message.platform;
        let reply = match arg.to_lowercase().as_str() {
            "on" => {
                self.store
                    .set_guild_setting(platform, guild, EMBEDS_KEY, None)?;
                "Alerts will be posted as an embed with a large image. (I need Embed Links; \
                 without it they fall back to plain text.)"
            }
            "off" => {
                self.store
                    .set_guild_setting(platform, guild, EMBEDS_KEY, Some("off"))?;
                "Alerts will be posted as plain text with the app's own link preview."
            }
            "" => {
                if self
                    .store
                    .guild_setting(platform, guild, EMBEDS_KEY)?
                    .as_deref()
                    == Some("off")
                {
                    "Alert embeds are off: plain text with the app's link preview."
                } else {
                    "Alert embeds are on (the default)."
                }
            }
            _ => return Err(Error::Usage(Self::usage(&ctx.prefix))),
        };
        ctx.reply(reply).await?;
        Ok(())
    }

    async fn moddm(&self, ctx: &CommandContext, guild: &str, arg: &str) -> Result<()> {
        let platform = ctx.message.platform;
        let reply = match arg.to_lowercase().as_str() {
            "on" => {
                self.store
                    .set_guild_setting(platform, guild, MOD_DM_KEY, Some("on"))?;
                "Members will get a DM saying what happened and why. (Some have DMs closed; the action still goes ahead.)"
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
        "Choose admin and mod roles, the mod-log channel, the mute role, mod DMs, alert embeds and the role new members get (server owner)."
    }

    fn level(&self) -> Level {
        Level::ServerOwner
    }

    fn subcommands(&self, _level: Level) -> Vec<(&'static str, &'static str)> {
        SET_OPTIONS.to_vec()
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
        if matches!(what.as_str(), "muterole" | "moddm" | "embeds" | "autorole") {
            let arg = ctx.args.trim_start()[what.len()..].trim().to_owned();
            return match what.as_str() {
                "muterole" => self.muterole(&ctx, &guild, &arg).await,
                "moddm" => self.moddm(&ctx, &guild, &arg).await,
                "autorole" => self.autorole(&ctx, &guild, &arg).await,
                _ => self.embeds(&ctx, &guild, &arg).await,
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

/// `admin addrole | removerole | announce | export | import`. `twitch_ready`: this bot polls
/// Twitch (imports warn otherwise).
pub fn admin_group(store: Arc<Store>, http: reqwest::Client, twitch_ready: bool) -> Group {
    Group::new(
        "admin",
        "Admin",
        "Admin tools: addrole, removerole, announce, export, import, structure (admins; import: server owner).",
    )
    .with(RoleCommand::add())
    .with(RoleCommand::remove())
    .with(AnnounceCommand)
    .with(crate::transfer::ExportCommand::new(store.clone()))
    .with(crate::structure::StructureCommand::new(http.clone()))
    .with(crate::transfer::ImportCommand::new(store, http, twitch_ready))
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
            "Give a role to one or more members (admins)."
        } else {
            "Take a role from a member (admins)."
        }
    }

    fn level(&self) -> Level {
        Level::Admin
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let (role_arg, users) = split_role_users(&ctx.args);
        // addrole takes several members; removerole exactly one.
        let usage = || {
            let who = if self.add {
                "<@user> [@user …]"
            } else {
                "<@user>"
            };
            Error::Usage(format!(
                "Usage: `{}{} <role> {who}` (role by mention, ID or name)",
                ctx.prefix,
                self.name()
            ))
        };
        if users.is_empty() || role_arg.is_empty() || (!self.add && users.len() > 1) {
            return Err(usage());
        }
        if users.len() > ROLE_USERS_MAX {
            return Err(Error::Usage(format!(
                "Up to {ROLE_USERS_MAX} members at a time."
            )));
        }
        let role = resolve_role(&ctx, &guild, &role_arg).await?;
        // Roles carrying Administrator: only the server owner (or a bot owner) may hand them out.
        if self.add && role.permissions & ADMINISTRATOR != 0 && ctx.level < Level::ServerOwner {
            return Err(Error::Usage(format!(
                "**{}** has the Administrator permission; only the server owner can give it out.",
                role.name
            )));
        }
        let (mut done, mut failed) = (Vec::new(), Vec::new());
        for user in &users {
            let result = if self.add {
                ctx.chat.add_role(&guild, user, &role.id).await
            } else {
                ctx.chat.remove_role(&guild, user, &role.id).await
            };
            // Names, not mentions: nobody gets pinged about a role change.
            let name = match ctx.chat.user_name(user).await {
                Ok(n) => format!("**{}**", escape_md(&n)),
                Err(_) => format!("`{user}`"),
            };
            match result {
                Ok(()) => done.push(name),
                Err(e) => {
                    tracing::info!(error = %e, role = %role.name, user, "role change refused");
                    failed.push(name);
                }
            }
        }
        let role_name = escape_md(&role.name);
        let mut reply = String::new();
        if !done.is_empty() {
            let (verb, dir) = if self.add {
                ("Gave", "to")
            } else {
                ("Took", "from")
            };
            reply.push_str(&format!(
                "{verb} **{role_name}** {dir} {}.",
                done.join(", ")
            ));
        }
        if !failed.is_empty() {
            // Usually the same cause for everyone: work it out once.
            let why = role_refusal(&*ctx.chat, &guild, &role).await;
            if !reply.is_empty() {
                reply.push('\n');
            }
            reply.push_str(&format!(
                "I couldn't change **{role_name}** for {}. {why}",
                failed.join(", ")
            ));
        }
        ctx.reply(&reply).await?;
        Ok(())
    }
}

/// Most members one `addrole`/`removerole` changes.
const ROLE_USERS_MAX: usize = 25;

/// `"<role…> <@user> [@user …]"` → `(role, users)`. The role is a mention, ID or name (names
/// may have spaces); every trailing word that is a user mention or ID is a member, but the first
/// word always belongs to the role.
fn split_role_users(args: &str) -> (String, Vec<String>) {
    let words: Vec<&str> = args.split_whitespace().collect();
    let mut cut = words.len();
    while cut > 1 && parse_user(words[cut - 1]).is_some() {
        cut -= 1;
    }
    let mut users: Vec<String> = words[cut..].iter().filter_map(|w| parse_user(w)).collect();
    users.dedup();
    (words[..cut].join(" "), users)
}

/// Why the platform probably refused a role change, with the fix: the bot's highest role isn't
/// above `role`, or none of the bot's roles has Manage Roles. Falls back to both hints.
async fn role_refusal(chat: &dyn ChatAdapter, guild: &str, role: &RoleInfo) -> String {
    let generic =
        "I need Manage Roles, and my own highest role must be above it in the server's role list."
            .to_owned();
    let (Ok(bot), Ok(roles)) = (chat.bot_user_id().await, chat.roles(guild).await) else {
        return generic;
    };
    let Ok(mine) = chat.member_roles(guild, &bot).await else {
        return generic;
    };
    let my_roles: Vec<&RoleInfo> = roles
        .iter()
        .filter(|r| mine.contains(&r.id) || r.id == guild)
        .collect();
    let perms = my_roles.iter().fold(0, |p, r| p | r.permissions);
    let top = my_roles
        .iter()
        .filter(|r| r.id != guild)
        .max_by_key(|r| r.position);
    match top {
        Some(top) if top.position <= role.position => format!(
            "My highest role, **{}**, is not above **{}**. Move **{}** above it in the server's role settings.",
            top.name, role.name, top.name
        ),
        None => format!(
            "I have no role of my own above **{}**. Give me a role with Manage Roles that's above it.",
            role.name
        ),
        Some(top) if perms & (MANAGE_ROLES | ADMINISTRATOR) == 0 => format!(
            "None of my roles has the **Manage Roles** permission. Turn it on for **{}**.",
            top.name
        ),
        Some(_) => generic,
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

    #[test]
    fn prefixes() {
        use super::valid_prefix;
        assert!(valid_prefix("?"));
        assert!(valid_prefix("bot!"));
        assert!(!valid_prefix(""));
        assert!(!valid_prefix("a b"));
        assert!(!valid_prefix("toolong"));
        assert!(!valid_prefix("`"));
    }

    #[test]
    fn role_then_members() {
        let users = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            split_role_users("Testing Role <@1> <@!2>"),
            ("Testing Role".to_owned(), users(&["1", "2"]))
        );
        assert_eq!(
            split_role_users("<@&9> <@1>"),
            ("<@&9>".to_owned(), users(&["1"]))
        );
        // A role given by ID: the first word stays the role.
        assert_eq!(
            split_role_users("9 1 2"),
            ("9".to_owned(), users(&["1", "2"]))
        );
        assert_eq!(
            split_role_users("Testing Role"),
            ("Testing Role".to_owned(), users(&[]))
        );
        assert_eq!(split_role_users("<@1>"), ("<@1>".to_owned(), users(&[])));
    }

    use super::{parse_user, split_role_users};

    #[test]
    fn user_arguments() {
        assert_eq!(parse_user("<@123>").as_deref(), Some("123"));
        assert_eq!(parse_user("<@!123>").as_deref(), Some("123"));
        assert_eq!(parse_user("123").as_deref(), Some("123"));
        assert_eq!(parse_user("<@&123>"), None);
        assert_eq!(parse_user("bob"), None);
    }
}
