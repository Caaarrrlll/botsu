//! Moderation (phase 5): warnings, kick, ban, temp-ban, timeout, numbered cases and a mod-log.
//!
//! | Command | Level |
//! |---|---|
//! | `warn`, `kick`, `ban`, `tempban`, `unban`, `timeout`, `untimeout`, `mute`, `unmute` | mod |
//! | `case`, `cases`, `reason`, `unwarn`, `bans` | mod |
//! | `warnings` (yourself) | anyone; someone else: mod |
//!
//! Every action is recorded as a case, numbered per server, and posted to the mod-log channel
//! when one is set (`set modlog #channel`). Temp-bans are normal bans plus a row in
//! `scheduled_unbans`; a background task lifts them when they're due, including ones that came
//! due while the bot was down. Timeouts expire on the platform's side.
//!
//! `mute` gives the server's mute role (`set muterole`; the role's channel permissions are the
//! server's business). With a duration, `scheduled_unmutes` takes it off again, through the same
//! task as temp-bans. Without a mute role, `mute` is a timeout and needs a duration.
//!
//! With `set moddm on`, the member gets a DM saying what happened and why before the action
//! (kick and ban have to come first: afterwards the bot may not share a server with them).
//!
//! Hierarchy: nobody can act on themselves, the bot or the server owner. Below the server owner,
//! the target must be below the moderator in both botsu's levels (a mod can't kick an admin)
//! and the server's role ranking. The platform enforces the bot's own rank and permissions.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::adapter::{ChatAdapter, Platform};
use crate::bot::Bot;
use crate::command::{Command, CommandContext, Group};
use crate::error::{Error, Result};
use crate::permissions::{Level, Permissions};
use crate::server::{guild, parse_user, CleanupCommand};
use crate::storage::{now, Case, NewCase, ScheduledUnban, ScheduledUnmute, Store};

/// `guild_settings` key for the mod-log channel.
pub const MODLOG_KEY: &str = "modlog_channel";
/// `guild_settings` key for the mute role.
pub const MUTE_ROLE_KEY: &str = "mute_role";
/// `guild_settings` key: `on` to DM members about actions taken against them.
pub const MOD_DM_KEY: &str = "mod_dm";

/// Platforms cap audit-log reasons at 512 characters.
const MAX_REASON: usize = 512;
const MIN_DURATION: i64 = 60;
const MAX_TEMPBAN: i64 = 366 * 86_400;
/// Discord's limit (Fluxer allows a year); the lower one applies everywhere.
const MAX_TIMEOUT: i64 = 28 * 86_400;
/// How often due temp-bans are checked, and how often lifting one is retried before giving up.
const UNBAN_CHECK_EVERY: Duration = Duration::from_secs(30);
const UNBAN_MAX_ATTEMPTS: i64 = 60;
/// Room left under the 2000-character message limit.
const MAX_MESSAGE: usize = 1900;

// --- durations and times --------------------------------------------------------------------------

/// `"90m"`, `"1d12h"`, `"2 weeks"` → seconds. Units: s, m, h, d, w (and their words).
pub fn parse_duration(s: &str) -> Option<i64> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let mut total: i64 = 0;
    let mut chars = s.chars().peekable();
    let mut any = false;
    while chars.peek().is_some() {
        let mut num = String::new();
        while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
            num.push(c);
            chars.next();
        }
        let mut unit = String::new();
        while let Some(c) = chars.peek().copied().filter(char::is_ascii_alphabetic) {
            unit.push(c);
            chars.next();
        }
        let secs: i64 = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86_400,
            "w" | "week" | "weeks" => 604_800,
            _ => return None,
        };
        let n: i64 = num.parse().ok()?;
        total = total.checked_add(n.checked_mul(secs)?)?;
        any = true;
    }
    (any && total > 0).then_some(total)
}

/// Seconds → `"1d 12h"` (the two largest units).
pub fn format_duration(secs: i64) -> String {
    let units = [
        ("w", 604_800),
        ("d", 86_400),
        ("h", 3600),
        ("m", 60),
        ("s", 1),
    ];
    let mut left = secs.max(0);
    let mut parts = Vec::new();
    for (name, size) in units {
        if left >= size && parts.len() < 2 {
            parts.push(format!("{}{name}", left / size));
            left %= size;
        } else if !parts.is_empty() {
            break;
        }
    }
    if parts.is_empty() {
        "0s".into()
    } else {
        parts.join(" ")
    }
}

/// `2026-11-20 13:05 UTC`, plus Discord's local-time tag (Fluxer may not render it).
fn format_time(platform: Platform, unix: i64) -> String {
    let Ok(t) = OffsetDateTime::from_unix_timestamp(unix) else {
        return unix.to_string();
    };
    let mut s = format!(
        "{}-{:02}-{:02} {:02}:{:02} UTC",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    );
    if platform == Platform::Discord {
        s.push_str(&format!(" (<t:{unix}:R>)"));
    }
    s
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

/// `"word rest of it"` → `("word", "rest of it")`.
fn split_first(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim()),
        None => (s, ""),
    }
}

/// Joins lines, stopping before the message gets too long.
fn fit(header: String, lines: impl IntoIterator<Item = String>) -> String {
    let mut out = header;
    for line in lines {
        if out.len() + line.len() + 1 > MAX_MESSAGE {
            out.push_str("\n…");
            break;
        }
        out.push('\n');
        out.push_str(&line);
    }
    out
}

// --- actions --------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Warn,
    Kick,
    Ban,
    TempBan,
    Unban,
    Timeout,
    Untimeout,
    Mute,
    Unmute,
}

impl Action {
    pub const ALL: [Action; 9] = [
        Action::Warn,
        Action::Kick,
        Action::Ban,
        Action::TempBan,
        Action::Unban,
        Action::Timeout,
        Action::Untimeout,
        Action::Mute,
        Action::Unmute,
    ];

    /// The command name, also stored as the case's action.
    fn key(self) -> &'static str {
        match self {
            Action::Warn => "warn",
            Action::Kick => "kick",
            Action::Ban => "ban",
            Action::TempBan => "tempban",
            Action::Unban => "unban",
            Action::Timeout => "timeout",
            Action::Untimeout => "untimeout",
            Action::Mute => "mute",
            Action::Unmute => "unmute",
        }
    }

    /// What the bot needs on the platform to do it.
    fn permission(self) -> &'static str {
        match self {
            Action::Warn => "",
            Action::Kick => "Kick Members",
            Action::Ban | Action::TempBan | Action::Unban => "Ban Members",
            Action::Timeout | Action::Untimeout => "Moderate Members (Timeout Members)",
            Action::Mute | Action::Unmute => "Manage Roles (above the mute role)",
        }
    }

    /// The DM sent to the member when `set moddm` is on; `None` for actions that don't get one.
    fn dm(self, server: &str, duration: Option<i64>, reason: Option<&str>) -> Option<String> {
        let what = match self {
            Action::Warn => "You were warned in",
            Action::Kick => "You were kicked from",
            Action::Ban | Action::TempBan => "You were banned from",
            Action::Timeout => "You were timed out in",
            Action::Mute => "You were muted in",
            Action::Unban | Action::Untimeout | Action::Unmute => return None,
        };
        let mut text = format!("{what} **{server}**");
        if let Some(d) = duration {
            text.push_str(&format!(" for {}", format_duration(d)));
        }
        text.push('.');
        if let Some(r) = reason {
            text.push_str(&format!("\nReason: {r}"));
        }
        Some(text)
    }
}

/// A stored action key → a heading.
fn title(key: &str) -> &str {
    match key {
        "warn" => "Warning",
        "kick" => "Kick",
        "ban" => "Ban",
        "tempban" => "Temp-ban",
        "unban" => "Unban",
        "timeout" => "Timeout",
        "untimeout" => "Timeout removed",
        "mute" => "Mute",
        "unmute" => "Unmute",
        other => other,
    }
}

// --- shared tools ---------------------------------------------------------------------------------

/// What all moderation commands share: storage, permission levels, the mod-log.
pub struct ModTools {
    store: Arc<Store>,
    perms: Arc<Permissions>,
}

impl ModTools {
    pub fn new(store: Arc<Store>, perms: Arc<Permissions>) -> Arc<Self> {
        Arc::new(Self { store, perms })
    }

    async fn name(&self, chat: &dyn ChatAdapter, user: &str) -> String {
        if user.is_empty() {
            return "botsu (automatic)".into();
        }
        chat.user_name(user)
            .await
            .unwrap_or_else(|_| format!("user {user}"))
    }

    /// Refuses targets the moderator mustn't act on (see the module docs).
    async fn check_target(&self, ctx: &CommandContext, guild: &str, target: &str) -> Result<()> {
        let refuse = |m: &str| Err(Error::Usage(m.to_owned()));
        let chat = &*ctx.chat;
        if target == ctx.message.author.id {
            return refuse("You can't do that to yourself.");
        }
        if chat.bot_user_id().await.is_ok_and(|me| me == target) {
            return refuse("I'm not doing that to myself.");
        }
        if let Ok(Some(owner)) = chat.guild_owner(guild).await {
            if owner == target {
                return refuse("Nobody can do that to the server owner.");
            }
        }
        if ctx.level >= Level::ServerOwner {
            return Ok(());
        }
        // Not a member (banning by ID): there's no rank to compare.
        let Ok(target_roles) = chat.member_roles(guild, target).await else {
            return Ok(());
        };
        let target_level = self.perms.level(chat, Some(guild), target, false).await;
        if target_level >= ctx.level {
            return Err(Error::Usage(format!(
                "They're {target_level}, so you can't do that to them."
            )));
        }
        let author_roles = chat.member_roles(guild, &ctx.message.author.id).await?;
        let roles = chat.roles(guild).await?;
        let top = |ids: &[String]| {
            roles
                .iter()
                .filter(|r| ids.contains(&r.id))
                .map(|r| r.position)
                .max()
                .unwrap_or(0)
        };
        if top(&target_roles) >= top(&author_roles) {
            return refuse("Their highest role is at or above yours.");
        }
        Ok(())
    }

    /// Stores a case and posts it to the mod-log. Returns its number.
    async fn record(&self, chat: &dyn ChatAdapter, case: NewCase<'_>) -> Result<i64> {
        let number = self.store.add_case(&case)?;
        if let Some(stored) = self.store.case(case.platform, case.guild_id, number)? {
            let text = self.describe(chat, &stored).await;
            self.post_modlog(chat, case.guild_id, &text).await;
        }
        Ok(number)
    }

    async fn post_modlog(&self, chat: &dyn ChatAdapter, guild: &str, text: &str) {
        let channel = match self.store.guild_setting(chat.platform(), guild, MODLOG_KEY) {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't read the mod-log setting");
                return;
            }
        };
        if let Err(e) = chat.send_message(&channel, text).await {
            tracing::warn!(error = %e, guild, channel = %channel, "couldn't post to the mod-log");
        }
    }

    /// A case as a message. Uses names, not mentions, so mod-log posts don't ping anyone.
    async fn describe(&self, chat: &dyn ChatAdapter, case: &Case) -> String {
        let platform = chat.platform();
        let mut s = format!(
            "**Case #{} · {}**\nUser: {} (`{}`)\nModerator: {}\nReason: {}\nWhen: {}",
            case.number,
            title(&case.action),
            self.name(chat, &case.target_id).await,
            case.target_id,
            self.name(chat, &case.moderator_id).await,
            case.reason.as_deref().unwrap_or("none given"),
            format_time(platform, case.created_at),
        );
        if let Some(until) = case.expires_at {
            s.push_str(&format!("\nUntil: {}", format_time(platform, until)));
        }
        if let Some(at) = case.revoked_at {
            s.push_str(&format!("\nRemoved: {}", format_time(platform, at)));
        }
        s
    }

    fn case_line(&self, platform: Platform, case: &Case) -> String {
        let mut line = format!(
            "#{} {} · {}",
            case.number,
            title(&case.action),
            format_time(platform, case.created_at),
        );
        if let Some(r) = &case.reason {
            line.push_str(&format!(" · {}", truncate(r, 80)));
        }
        if case.revoked_at.is_some() {
            line.push_str(" (removed)");
        }
        line
    }

    /// Lifts temp-bans that are due. Run by [`spawn_unban_scheduler`]; public for tests.
    pub async fn lift_due_bans(&self, bot: &Bot) {
        let due = match self.store.due_unbans(now()) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't read due temp-bans");
                return;
            }
        };
        for u in due {
            // Not connected (yet): try again next round.
            let Some(chat) = bot.chat(u.platform) else {
                continue;
            };
            if let Err(e) = self.lift(&*chat, &u).await {
                tracing::warn!(error = %e, guild = %u.guild_id, user = %u.user_id, "temp-ban expiry failed");
            }
        }
    }

    async fn lift(&self, chat: &dyn ChatAdapter, u: &ScheduledUnban) -> Result<()> {
        let reason = format!("Temp-ban (case #{}) ended", u.case_number);
        let (platform, guild, user) = (u.platform, u.guild_id.as_str(), u.user_id.as_str());
        match chat.unban(guild, user, Some(&reason)).await {
            Ok(()) => {
                self.store.cancel_unban(platform, guild, user)?;
                self.record(
                    chat,
                    NewCase {
                        platform,
                        guild_id: guild,
                        action: Action::Unban.key(),
                        target_id: user,
                        moderator_id: "",
                        reason: Some(&reason),
                        expires_at: None,
                    },
                )
                .await?;
                tracing::info!(%platform, guild, user, "temp-ban ended");
            }
            Err(e) => {
                // Unbanned by hand in the meantime: nothing left to do.
                let still_banned = chat
                    .bans(guild)
                    .await
                    .map(|bans| bans.iter().any(|b| b.user_id == user))
                    .unwrap_or(true);
                if !still_banned {
                    self.store.cancel_unban(platform, guild, user)?;
                    return Ok(());
                }
                if u.attempts + 1 >= UNBAN_MAX_ATTEMPTS {
                    self.store.cancel_unban(platform, guild, user)?;
                    let name = self.name(chat, user).await;
                    let text = format!(
                        "I couldn't lift the temp-ban from case #{} on {name} (`{user}`) and gave \
                         up: {e}. Please unban them by hand.",
                        u.case_number
                    );
                    self.post_modlog(chat, guild, &text).await;
                } else {
                    self.store.note_unban_attempt(platform, guild, user)?;
                }
                return Err(e);
            }
        }
        Ok(())
    }
}

impl ModTools {
    /// Takes the mute role off members whose mute is due. Run with the temp-ban check.
    pub async fn lift_due_mutes(&self, bot: &Bot) {
        let due = match self.store.due_unmutes(now()) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't read due mutes");
                return;
            }
        };
        for u in due {
            let Some(chat) = bot.chat(u.platform) else {
                continue;
            };
            if let Err(e) = self.unmute_due(&*chat, &u).await {
                tracing::warn!(error = %e, guild = %u.guild_id, user = %u.user_id, "mute expiry failed");
            }
        }
    }

    async fn unmute_due(&self, chat: &dyn ChatAdapter, u: &ScheduledUnmute) -> Result<()> {
        let reason = format!("Mute (case #{}) ended", u.case_number);
        let (platform, guild, user) = (u.platform, u.guild_id.as_str(), u.user_id.as_str());
        match chat.remove_role(guild, user, &u.role_id).await {
            Ok(()) => {
                self.store.cancel_unmute(platform, guild, user)?;
                self.record(
                    chat,
                    NewCase {
                        platform,
                        guild_id: guild,
                        action: Action::Unmute.key(),
                        target_id: user,
                        moderator_id: "",
                        reason: Some(&reason),
                        expires_at: None,
                    },
                )
                .await?;
                tracing::info!(%platform, guild, user, "mute ended");
                Ok(())
            }
            Err(e) => {
                // Left the server, or the role is already gone: nothing left to do.
                let still_muted = chat
                    .member_roles(guild, user)
                    .await
                    .map(|roles| roles.contains(&u.role_id))
                    .unwrap_or(false);
                if !still_muted {
                    self.store.cancel_unmute(platform, guild, user)?;
                    return Ok(());
                }
                if u.attempts + 1 >= UNBAN_MAX_ATTEMPTS {
                    self.store.cancel_unmute(platform, guild, user)?;
                    let name = self.name(chat, user).await;
                    let text = format!(
                        "I couldn't end the mute from case #{} on {name} (`{user}`) and gave \
                         up: {e}. Please remove the mute role by hand.",
                        u.case_number
                    );
                    self.post_modlog(chat, guild, &text).await;
                } else {
                    self.store.note_unmute_attempt(platform, guild, user)?;
                }
                Err(e)
            }
        }
    }

    /// DMs the member about an action if the server turned that on. Returns a note for the
    /// moderator's reply when the DM couldn't be sent.
    async fn notify(
        &self,
        chat: &dyn ChatAdapter,
        guild: &str,
        target: &str,
        action: Action,
        duration: Option<i64>,
        reason: Option<&str>,
    ) -> &'static str {
        let on = matches!(
            self.store.guild_setting(chat.platform(), guild, MOD_DM_KEY),
            Ok(Some(v)) if v == "on"
        );
        if !on {
            return "";
        }
        let server = chat
            .guild_name(guild)
            .await
            .unwrap_or_else(|_| "the server".to_owned());
        let Some(text) = action.dm(&server, duration, reason) else {
            return "";
        };
        match chat.send_dm(target, &text).await {
            Ok(()) => "",
            Err(e) => {
                tracing::info!(error = %e, user = target, "moderation DM not delivered");
                " (I couldn't DM them.)"
            }
        }
    }
}

/// Checks for due temp-bans and mutes every 30 s, starting right away (catching up after
/// downtime).
pub fn spawn_unban_scheduler(bot: Arc<Bot>, tools: Arc<ModTools>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(UNBAN_CHECK_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            tools.lift_due_bans(&bot).await;
            tools.lift_due_mutes(&bot).await;
        }
    })
}

/// `mod <command>`: cleanup and every moderation command (alias `moderation`).
pub fn group(tools: &Arc<ModTools>) -> Group {
    let mut group = Group::new(
        "mod",
        "Moderation",
        "Moderation: cleanup, warn, kick, ban, timeout, cases, … (mods; warnings: anyone).",
    )
    .alias("moderation")
    .with(CleanupCommand)
    .with(CaseCommand(tools.clone()))
    .with(CasesCommand(tools.clone()))
    .with(ReasonCommand(tools.clone()))
    .with(WarningsCommand(tools.clone()))
    .with(UnwarnCommand(tools.clone()))
    .with(BansCommand(tools.clone()));
    for command in ModCommand::all(tools) {
        group = group.with(command);
    }
    group
}

// --- warn / kick / ban / tempban / unban / timeout / untimeout ------------------------------------

pub struct ModCommand {
    action: Action,
    tools: Arc<ModTools>,
}

impl ModCommand {
    /// One command per [`Action`].
    pub fn all(tools: &Arc<ModTools>) -> Vec<Self> {
        Action::ALL
            .into_iter()
            .map(|action| Self {
                action,
                tools: tools.clone(),
            })
            .collect()
    }

    fn usage(&self, p: &str) -> String {
        let n = self.action.key();
        match self.action {
            Action::Warn => format!("Usage: `{p}{n} <@user> <reason>`"),
            Action::Ban => {
                format!("Usage: `{p}{n} <@user or ID> [days of messages to delete, 0-7] [reason]`")
            }
            Action::TempBan => format!(
                "Usage: `{p}{n} <@user> <duration> [reason]`, for example `{p}{n} @someone 3d \
                 spam`. Units: m, h, d, w."
            ),
            Action::Unban => format!("Usage: `{p}{n} <user ID> [reason]`"),
            Action::Timeout => format!(
                "Usage: `{p}{n} <@user> <duration> [reason]`, for example `{p}{n} @someone 10m`. \
                 Up to 28 days."
            ),
            Action::Mute => format!(
                "Usage: `{p}{n} <@user> [duration] [reason]`. Uses the mute role \
                 (`{p}set muterole`); without one it's a timeout and needs a duration."
            ),
            Action::Kick | Action::Untimeout | Action::Unmute => {
                format!("Usage: `{p}{n} <@user> [reason]`")
            }
        }
    }
}

#[async_trait]
impl Command for ModCommand {
    fn name(&self) -> &'static str {
        self.action.key()
    }

    fn help(&self) -> &'static str {
        match self.action {
            Action::Warn => "Warn a member; it's kept as a case (mods).",
            Action::Kick => "Kick a member (mods).",
            Action::Ban => "Ban a user, optionally deleting recent messages (mods).",
            Action::TempBan => "Ban a user for a while; lifted automatically (mods).",
            Action::Unban => "Lift a ban (mods).",
            Action::Timeout => "Time a member out, up to 28 days (mods).",
            Action::Untimeout => "End a member's timeout (mods).",
            Action::Mute => "Mute a member with the mute role, optionally for a while (mods).",
            Action::Unmute => "Take the mute role (or a timeout) off a member (mods).",
        }
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let usage = || Error::Usage(self.usage(&ctx.prefix));
        let (who, mut rest) = split_first(&ctx.args);
        let target = parse_user(who).ok_or_else(usage)?;

        let platform = ctx.message.platform;
        let mute_role = match self.action {
            Action::Mute | Action::Unmute => {
                self.tools
                    .store
                    .guild_setting(platform, &guild, MUTE_ROLE_KEY)?
            }
            _ => None,
        };
        let mut duration = None;
        let mut days = 0u8;
        match self.action {
            Action::Mute => {
                // Optional duration; required (and capped) when it falls back to a timeout.
                let (d, r) = split_first(rest);
                if let Some(secs) = parse_duration(d) {
                    let max = if mute_role.is_some() {
                        MAX_TEMPBAN
                    } else {
                        MAX_TIMEOUT
                    };
                    if !(MIN_DURATION..=max).contains(&secs) {
                        return Err(Error::Usage(format!(
                            "The duration must be between 1 minute and {}.",
                            format_duration(max)
                        )));
                    }
                    duration = Some(secs);
                    rest = r;
                } else if mute_role.is_none() {
                    return Err(Error::Usage(format!(
                        "There's no mute role here, so `{p}mute` is a timeout and needs a \
                         duration (up to 28 days), for example `{p}mute @someone 1h`. A server \
                         owner can set one with `{p}set muterole <role>`.",
                        p = ctx.prefix
                    )));
                }
            }
            Action::TempBan | Action::Timeout => {
                let (d, r) = split_first(rest);
                let secs = parse_duration(d).ok_or_else(usage)?;
                let max = if self.action == Action::Timeout {
                    MAX_TIMEOUT
                } else {
                    MAX_TEMPBAN
                };
                if !(MIN_DURATION..=max).contains(&secs) {
                    return Err(Error::Usage(format!(
                        "The duration must be between 1 minute and {}.",
                        format_duration(max)
                    )));
                }
                duration = Some(secs);
                rest = r;
            }
            Action::Ban => {
                let (d, r) = split_first(rest);
                if let Some(n) = d.parse::<u8>().ok().filter(|n| *n <= 7) {
                    days = n;
                    rest = r;
                }
            }
            _ => {}
        }
        let reason = (!rest.is_empty()).then(|| truncate(rest, MAX_REASON));
        if self.action == Action::Warn && reason.is_none() {
            return Err(usage());
        }
        if self.action != Action::Unban {
            self.tools.check_target(&ctx, &guild, &target).await?;
        }

        let chat = &*ctx.chat;
        let store = &self.tools.store;
        let name = self.tools.name(chat, &target).await;
        let r = reason.as_deref();
        let expires_at = duration.map(|d| now() + d);
        let dm_note = self
            .tools
            .notify(chat, &guild, &target, self.action, duration, r)
            .await;
        let result = match self.action {
            Action::Warn => Ok(()),
            Action::Kick => chat.kick(&guild, &target, r).await,
            Action::Ban => chat.ban(&guild, &target, r, days).await,
            Action::TempBan => chat.ban(&guild, &target, r, 0).await,
            Action::Unban => chat.unban(&guild, &target, r).await,
            Action::Timeout => chat.timeout(&guild, &target, expires_at, r).await,
            Action::Untimeout => chat.timeout(&guild, &target, None, r).await,
            Action::Mute => match &mute_role {
                Some(role) => chat.add_role(&guild, &target, role).await,
                None => chat.timeout(&guild, &target, expires_at, r).await,
            },
            Action::Unmute => match &mute_role {
                Some(role) => {
                    let removed = chat.remove_role(&guild, &target, role).await;
                    // Also end a timeout from before the mute role was set (best effort).
                    if removed.is_ok() {
                        let _ = chat.timeout(&guild, &target, None, r).await;
                    }
                    removed
                }
                None => chat.timeout(&guild, &target, None, r).await,
            },
        };
        if let Err(e) = result {
            if matches!(e, Error::Unsupported { .. }) {
                return Err(e);
            }
            tracing::info!(error = %e, action = self.action.key(), user = %target, "moderation action refused");
            let hint = if self.action == Action::Unban {
                format!(" Are they banned? I need **{}**.", self.action.permission())
            } else {
                format!(
                    " I need **{}**, and my highest role must be above theirs.",
                    self.action.permission()
                )
            };
            return Err(Error::Usage(format!(
                "I couldn't {} **{name}**.{hint}",
                self.action.key()
            )));
        }

        let number = self
            .tools
            .record(
                chat,
                NewCase {
                    platform,
                    guild_id: &guild,
                    action: self.action.key(),
                    target_id: &target,
                    moderator_id: &ctx.message.author.id,
                    reason: r,
                    expires_at,
                },
            )
            .await?;
        match self.action {
            Action::TempBan => store.schedule_unban(
                platform,
                &guild,
                &target,
                expires_at.unwrap_or_default(),
                number,
            )?,
            // A permanent ban or an unban replaces any temp-ban.
            Action::Ban | Action::Unban => {
                store.cancel_unban(platform, &guild, &target)?;
            }
            // A new mute replaces a scheduled end; a mute without one is open-ended.
            Action::Mute => {
                store.cancel_unmute(platform, &guild, &target)?;
                if let (Some(role), Some(at)) = (&mute_role, expires_at) {
                    store.schedule_unmute(&ScheduledUnmute {
                        platform,
                        guild_id: guild.clone(),
                        user_id: target.clone(),
                        role_id: role.clone(),
                        unmute_at: at,
                        case_number: number,
                        attempts: 0,
                    })?;
                }
            }
            Action::Unmute => {
                store.cancel_unmute(platform, &guild, &target)?;
            }
            _ => {}
        }

        let reply = match self.action {
            Action::Warn => {
                let count = store.cases_for(platform, &guild, &target, true)?.len();
                format!(
                    "<@{target}>, you've been warned: {}\n(case #{number}, warning {count})",
                    r.unwrap_or_default()
                )
            }
            Action::Kick => format!("Kicked **{name}**. (case #{number})"),
            Action::Ban => format!("Banned **{name}**. (case #{number})"),
            Action::TempBan | Action::Timeout => {
                let verb = if self.action == Action::TempBan {
                    "Banned"
                } else {
                    "Timed out"
                };
                format!(
                    "{verb} **{name}** for {}, until {}. (case #{number})",
                    format_duration(duration.unwrap_or_default()),
                    format_time(platform, expires_at.unwrap_or_default())
                )
            }
            Action::Unban => format!("Unbanned **{name}**. (case #{number})"),
            Action::Untimeout => format!("Ended **{name}**'s timeout. (case #{number})"),
            Action::Mute => match (duration, &mute_role) {
                (Some(d), Some(_)) => format!(
                    "Muted **{name}** for {}, until {}. (case #{number})",
                    format_duration(d),
                    format_time(platform, expires_at.unwrap_or_default())
                ),
                (None, _) => format!("Muted **{name}**. (case #{number})"),
                (Some(d), None) => format!(
                    "Timed out **{name}** for {} (no mute role set), until {}. (case #{number})",
                    format_duration(d),
                    format_time(platform, expires_at.unwrap_or_default())
                ),
            },
            Action::Unmute => format!("Unmuted **{name}**. (case #{number})"),
        };
        ctx.reply(&format!("{reply}{dm_note}")).await?;
        Ok(())
    }
}

// --- case / cases / reason ------------------------------------------------------------------------

fn case_number(arg: &str, usage: impl Fn() -> Error) -> Result<i64> {
    arg.trim_start_matches('#')
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(usage)
}

fn get_case(store: &Store, ctx: &CommandContext, guild: &str, number: i64) -> Result<Case> {
    store
        .case(ctx.message.platform, guild, number)?
        .ok_or_else(|| Error::Usage(format!("There's no case #{number} in this server.")))
}

/// `case <n>`: shows a case.
pub struct CaseCommand(pub Arc<ModTools>);

#[async_trait]
impl Command for CaseCommand {
    fn name(&self) -> &'static str {
        "case"
    }

    fn help(&self) -> &'static str {
        "Show a moderation case (mods)."
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let usage = || Error::Usage(format!("Usage: `{}case <number>`", ctx.prefix));
        let number = case_number(&ctx.args, usage)?;
        let case = get_case(&self.0.store, &ctx, &guild, number)?;
        let text = self.0.describe(&*ctx.chat, &case).await;
        ctx.reply(&text).await?;
        Ok(())
    }
}

/// `cases <user>`: a user's history in this server.
pub struct CasesCommand(pub Arc<ModTools>);

#[async_trait]
impl Command for CasesCommand {
    fn name(&self) -> &'static str {
        "cases"
    }

    fn help(&self) -> &'static str {
        "List a user's moderation cases (mods)."
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let user = parse_user(ctx.args.trim())
            .ok_or_else(|| Error::Usage(format!("Usage: `{}cases <@user or ID>`", ctx.prefix)))?;
        let platform = ctx.message.platform;
        let cases = self.0.store.cases_for(platform, &guild, &user, false)?;
        let name = self.0.name(&*ctx.chat, &user).await;
        if cases.is_empty() {
            ctx.reply(&format!("**{name}** has no cases here.")).await?;
            return Ok(());
        }
        let text = fit(
            format!("**{name}**: {} case(s), newest first", cases.len()),
            cases.iter().map(|c| self.0.case_line(platform, c)),
        );
        ctx.reply(&text).await?;
        Ok(())
    }
}

/// `reason <n> <text>`: sets a case's reason (its moderator, or an admin).
pub struct ReasonCommand(pub Arc<ModTools>);

#[async_trait]
impl Command for ReasonCommand {
    fn name(&self) -> &'static str {
        "reason"
    }

    fn help(&self) -> &'static str {
        "Change a case's reason (mods)."
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let usage = || {
            Error::Usage(format!(
                "Usage: `{}reason <case number> <text>`",
                ctx.prefix
            ))
        };
        let (n, text) = split_first(&ctx.args);
        let number = case_number(n, usage)?;
        if text.is_empty() {
            return Err(usage());
        }
        let case = get_case(&self.0.store, &ctx, &guild, number)?;
        if case.moderator_id != ctx.message.author.id && ctx.level < Level::Admin {
            return Err(Error::Usage(
                "Only the case's moderator or an admin can change its reason.".into(),
            ));
        }
        let platform = ctx.message.platform;
        self.0
            .store
            .set_case_reason(platform, &guild, number, &truncate(text, MAX_REASON))?;
        if let Some(updated) = self.0.store.case(platform, &guild, number)? {
            let text = format!(
                "Reason updated:\n{}",
                self.0.describe(&*ctx.chat, &updated).await
            );
            self.0.post_modlog(&*ctx.chat, &guild, &text).await;
        }
        ctx.reply(&format!("Updated the reason for case #{number}."))
            .await?;
        Ok(())
    }
}

// --- warnings / unwarn ----------------------------------------------------------------------------

/// `warnings [user]`: anyone sees their own; mods see anyone's.
pub struct WarningsCommand(pub Arc<ModTools>);

#[async_trait]
impl Command for WarningsCommand {
    fn name(&self) -> &'static str {
        "warnings"
    }

    fn help(&self) -> &'static str {
        "Show your warnings (mods: anyone's)."
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let me = ctx.message.author.id.clone();
        let user = if ctx.args.trim().is_empty() {
            me.clone()
        } else {
            parse_user(ctx.args.trim()).ok_or_else(|| {
                Error::Usage(format!("Usage: `{}warnings [@user or ID]`", ctx.prefix))
            })?
        };
        if user != me {
            let level = self
                .0
                .perms
                .level(&*ctx.chat, Some(&guild), &me, ctx.is_owner)
                .await;
            if level < Level::Mod {
                return Err(Error::Usage(
                    "You can only see your own warnings; mods can see anyone's.".into(),
                ));
            }
        }
        let platform = ctx.message.platform;
        let warnings = self.0.store.cases_for(platform, &guild, &user, true)?;
        let name = self.0.name(&*ctx.chat, &user).await;
        if warnings.is_empty() {
            ctx.reply(&format!("**{name}** has no warnings here."))
                .await?;
            return Ok(());
        }
        let text = fit(
            format!("**{name}**: {} warning(s)", warnings.len()),
            warnings.iter().map(|c| self.0.case_line(platform, c)),
        );
        ctx.reply(&text).await?;
        Ok(())
    }
}

/// `unwarn <case> [reason]`: removes a warning (the case stays, marked removed).
pub struct UnwarnCommand(pub Arc<ModTools>);

#[async_trait]
impl Command for UnwarnCommand {
    fn name(&self) -> &'static str {
        "unwarn"
    }

    fn help(&self) -> &'static str {
        "Remove a warning by its case number (mods)."
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let usage = || {
            Error::Usage(format!(
                "Usage: `{}unwarn <case number> [reason]` (`{}warnings @user` lists them)",
                ctx.prefix, ctx.prefix
            ))
        };
        let (n, why) = split_first(&ctx.args);
        let number = case_number(n, usage)?;
        let platform = ctx.message.platform;
        let case = get_case(&self.0.store, &ctx, &guild, number)?;
        if !self.0.store.revoke_warning(platform, &guild, number)? {
            return Err(Error::Usage(format!(
                "Case #{number} isn't a warning that's still in effect."
            )));
        }
        let chat = &*ctx.chat;
        let mut text = format!(
            "Warning #{number} for {} (`{}`) removed by {}.",
            self.0.name(chat, &case.target_id).await,
            case.target_id,
            self.0.name(chat, &ctx.message.author.id).await,
        );
        if !why.is_empty() {
            text.push_str(&format!(" Reason: {}", truncate(why, MAX_REASON)));
        }
        self.0.post_modlog(chat, &guild, &text).await;
        ctx.reply(&format!("Removed warning #{number}.")).await?;
        Ok(())
    }
}

// --- bans -----------------------------------------------------------------------------------------

/// `bans`: the server's ban list, with temp-ban end times.
pub struct BansCommand(pub Arc<ModTools>);

#[async_trait]
impl Command for BansCommand {
    fn name(&self) -> &'static str {
        "bans"
    }

    fn help(&self) -> &'static str {
        "List this server's bans (mods)."
    }

    fn level(&self) -> Level {
        Level::Mod
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let platform = ctx.message.platform;
        let bans = ctx.chat.bans(&guild).await.map_err(|e| {
            if matches!(e, Error::Unsupported { .. }) {
                return e;
            }
            tracing::info!(error = %e, "ban list refused");
            Error::Usage("I can't read the ban list; I need **Ban Members**.".into())
        })?;
        if bans.is_empty() {
            ctx.reply("Nobody is banned here.").await?;
            return Ok(());
        }
        let mut lines = Vec::new();
        for b in &bans {
            let mut line = format!("{} (`{}`)", b.user_name, b.user_id);
            if let Some(r) = &b.reason {
                line.push_str(&format!(": {}", truncate(r, 80)));
            }
            if let Some(at) = self.0.store.scheduled_unban(platform, &guild, &b.user_id)? {
                line.push_str(&format!(" · until {}", format_time(platform, at)));
            }
            lines.push(line);
        }
        ctx.reply(&fit(format!("**{} ban(s)**", bans.len()), lines))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Author, BanInfo, Capabilities, IncomingMessage, RoleInfo, ADMINISTRATOR};
    use crate::permissions::MOD_ROLE;
    use std::collections::HashSet;
    use std::sync::Mutex;

    #[test]
    fn dm_texts() {
        assert_eq!(
            Action::TempBan
                .dm("Srv", Some(3 * 86_400), Some("spam"))
                .as_deref(),
            Some(
                "You were banned from **Srv** for 3d.
Reason: spam"
            )
        );
        assert_eq!(
            Action::Mute.dm("Srv", None, None).as_deref(),
            Some("You were muted in **Srv**.")
        );
        assert_eq!(Action::Unmute.dm("Srv", None, None), None);
        assert_eq!(Action::Unban.dm("Srv", None, None), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90m"), Some(5400));
        assert_eq!(parse_duration("1d12h"), Some(129_600));
        assert_eq!(parse_duration("2 weeks"), Some(1_209_600));
        assert_eq!(parse_duration("1H"), Some(3600));
        assert_eq!(parse_duration("10"), None, "a unit is required");
        assert_eq!(parse_duration("3mo"), None);
        assert_eq!(parse_duration("d"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("0m"), None);
        assert_eq!(parse_duration("99999999999999999w"), None, "overflow");

        assert_eq!(format_duration(5400), "1h 30m");
        assert_eq!(format_duration(129_600), "1d 12h");
        assert_eq!(format_duration(86_400 * 28), "4w");
        assert_eq!(format_duration(90_061), "1d 1h");
        assert_eq!(format_duration(0), "0s");
    }

    #[test]
    fn times_and_text() {
        assert_eq!(
            format_time(Platform::Fluxer, 1_795_600_000),
            "2026-11-25 09:46 UTC"
        );
        assert!(format_time(Platform::Discord, 1_795_600_000).ends_with("(<t:1795600000:R>)"));
        assert_eq!(truncate("héllo", 2), "hé");
        assert_eq!(split_first("  a  b c "), ("a", "b c"));
        assert_eq!(split_first("a"), ("a", ""));
    }

    /// Server "g" owned by "1"; the bot is "2". Roles by rank: @everyone (0), "modr" (1,
    /// configured as mod), "adm" (2, Administrator). Members: "10" (modr), "11" (modr),
    /// "20" (adm), "30" (none). "40" isn't a member.
    #[derive(Default)]
    struct FakeChat {
        actions: Mutex<Vec<String>>,
        sent: Mutex<Vec<(String, String)>>,
        banned: Mutex<HashSet<String>>,
    }

    impl FakeChat {
        fn act(&self, a: String) {
            self.actions.lock().unwrap().push(a);
        }
        fn actions(&self) -> Vec<String> {
            self.actions.lock().unwrap().clone()
        }
        /// Everything sent to `channel`, oldest first, joined (help sends several messages).
        fn all_sent(&self, channel: &str) -> String {
            let sent = self.sent.lock().unwrap();
            sent.iter()
                .filter(|(c, _)| c == channel)
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join(
                    "
",
                )
        }

        fn last_sent(&self, channel: &str) -> String {
            let sent = self.sent.lock().unwrap();
            sent.iter()
                .rev()
                .find(|(c, _)| c == channel)
                .map(|(_, t)| t.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl ChatAdapter for FakeChat {
        fn platform(&self) -> Platform {
            Platform::Fluxer
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        async fn send_message(&self, channel: &str, text: &str) -> Result<String> {
            self.sent
                .lock()
                .unwrap()
                .push((channel.into(), text.into()));
            Ok("m".into())
        }
        async fn guild_owner(&self, _: &str) -> Result<Option<String>> {
            Ok(Some("1".into()))
        }
        async fn member_roles(&self, _: &str, user: &str) -> Result<Vec<String>> {
            match user {
                "10" | "11" => Ok(vec!["modr".into()]),
                "20" => Ok(vec!["adm".into()]),
                "30" | "1" | "2" => Ok(vec![]),
                _ => Err(Error::Internal("unknown member".into())),
            }
        }
        async fn roles(&self, guild: &str) -> Result<Vec<RoleInfo>> {
            let role = |id: &str, permissions, position| RoleInfo {
                id: id.into(),
                name: id.into(),
                permissions,
                position,
            };
            Ok(vec![
                role(guild, 0, 0),
                role("modr", 0, 1),
                role("adm", ADMINISTRATOR, 2),
            ])
        }
        async fn bot_user_id(&self) -> Result<String> {
            Ok("2".into())
        }
        async fn user_name(&self, user: &str) -> Result<String> {
            Ok(format!("name-{user}"))
        }
        async fn add_role(&self, _: &str, user: &str, role: &str) -> Result<()> {
            self.act(format!("addrole {user} {role}"));
            Ok(())
        }
        async fn kick(&self, _: &str, user: &str, _: Option<&str>) -> Result<()> {
            self.act(format!("kick {user}"));
            Ok(())
        }
        async fn ban(&self, _: &str, user: &str, _: Option<&str>, days: u8) -> Result<()> {
            self.act(format!("ban {user} {days}"));
            self.banned.lock().unwrap().insert(user.into());
            Ok(())
        }
        async fn unban(&self, _: &str, user: &str, _: Option<&str>) -> Result<()> {
            self.act(format!("unban {user}"));
            if self.banned.lock().unwrap().remove(user) {
                Ok(())
            } else {
                Err(Error::Internal("unknown ban".into()))
            }
        }
        async fn bans(&self, _: &str) -> Result<Vec<BanInfo>> {
            Ok(self
                .banned
                .lock()
                .unwrap()
                .iter()
                .map(|u| BanInfo {
                    user_id: u.clone(),
                    user_name: u.clone(),
                    reason: None,
                })
                .collect())
        }
        async fn timeout(
            &self,
            _: &str,
            user: &str,
            until: Option<i64>,
            _: Option<&str>,
        ) -> Result<()> {
            self.act(format!("timeout {user} {}", until.is_some()));
            Ok(())
        }
    }

    struct Setup {
        bot: Arc<Bot>,
        chat: Arc<FakeChat>,
        store: Arc<Store>,
        tools: Arc<ModTools>,
    }

    fn setup() -> Setup {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store
            .add_guild_role(Platform::Fluxer, "g", MOD_ROLE, "modr")
            .unwrap();
        store
            .set_guild_setting(Platform::Fluxer, "g", MODLOG_KEY, Some("log"))
            .unwrap();
        let perms = Arc::new(Permissions::new(store.clone()));
        let tools = ModTools::new(store.clone(), perms.clone());
        let mut bot = Bot::new("!", Vec::<String>::new());
        bot.set_permissions(perms);
        bot.register(group(&tools));
        let chat = Arc::new(FakeChat::default());
        let bot = Arc::new(bot);
        bot.register_chat(chat.clone());
        Setup {
            bot,
            chat,
            store,
            tools,
        }
    }

    impl Setup {
        async fn say(&self, author: &str, content: &str) -> String {
            let message = IncomingMessage {
                platform: Platform::Fluxer,
                guild_id: Some("g".into()),
                channel_id: "here".into(),
                message_id: "1".into(),
                author: Author {
                    id: author.into(),
                    name: author.into(),
                    is_bot: false,
                },
                content: content.into(),
                attachments: Vec::new(),
            };
            self.bot.handle_message(self.chat.clone(), message).await;
            self.chat.last_sent("here")
        }
    }

    #[tokio::test]
    async fn hierarchy() {
        let s = setup();
        assert!(s
            .say("30", "!mod kick 10")
            .await
            .contains("You need to be a mod"));
        let r = s.say("10", "!mod kick 10").await;
        assert!(r.contains("yourself"), "{r}");
        assert!(s.say("10", "!mod kick 2").await.contains("myself"));
        assert!(s.say("10", "!mod kick 1").await.contains("server owner"));
        assert!(s.say("10", "!mod kick 20").await.contains("an admin"));
        assert!(
            s.say("10", "!mod kick 11").await.contains("They're a mod"),
            "same level"
        );
        assert!(s.chat.actions().is_empty());

        assert!(s
            .say("20", "!mod kick 10 spam")
            .await
            .contains("Kicked **name-10**"));
        assert!(s.say("1", "!mod kick 20").await.contains("Kicked"));
        assert_eq!(s.chat.actions(), ["kick 10", "kick 20"]);
        // Banning someone who isn't a member works (no rank to compare).
        assert!(s.say("10", "!mod ban 40 7 raid").await.contains("Banned"));
        assert_eq!(s.chat.actions()[2], "ban 40 7");
    }

    #[tokio::test]
    async fn warnings_cases_and_modlog() {
        let s = setup();
        assert!(
            s.say("10", "!mod warn 30").await.contains("Usage"),
            "reason required"
        );
        let reply = s.say("10", "!mod warn 30 be nice").await;
        assert!(
            reply.contains("<@30>") && reply.contains("case #1, warning 1"),
            "{reply}"
        );
        let log = s.chat.last_sent("log");
        assert!(
            log.contains("Case #1 · Warning") && log.contains("name-30"),
            "{log}"
        );
        assert!(!log.contains("<@"), "mod-log posts don't ping");

        s.say("10", "!mod warn 30 again").await;
        assert!(s.say("30", "!mod warnings").await.contains("2 warning(s)"));
        assert!(s
            .say("30", "!mod warnings 10")
            .await
            .contains("only see your own"));
        assert!(s
            .say("10", "!mod unwarn 1")
            .await
            .contains("Removed warning #1"));
        assert!(s
            .say("10", "!mod unwarn 1")
            .await
            .contains("isn't a warning"));
        assert!(s
            .say("10", "!mod warnings 30")
            .await
            .contains("1 warning(s)"));

        assert!(s
            .say("11", "!mod reason 2 typo")
            .await
            .contains("moderator or an admin"));
        assert!(s
            .say("10", "!mod reason 2 spamming links")
            .await
            .contains("Updated"));
        let case = s.store.case(Platform::Fluxer, "g", 2).unwrap().unwrap();
        assert_eq!(case.reason.as_deref(), Some("spamming links"));
    }

    #[tokio::test]
    async fn tempban_is_lifted_when_due() {
        let s = setup();
        assert!(s.say("10", "!mod tempban 30 10").await.contains("Usage"));
        assert!(s
            .say("10", "!mod tempban 30 30s")
            .await
            .contains("between 1 minute"));
        let reply = s.say("10", "!mod tempban 30 2h flooding").await;
        assert!(reply.contains("for 2h, until"), "{reply}");
        let at = s
            .store
            .scheduled_unban(Platform::Fluxer, "g", "30")
            .unwrap()
            .unwrap();
        assert!((at - now() - 7200).abs() < 5);

        // Not due yet.
        s.tools.lift_due_bans(&s.bot).await;
        assert_eq!(s.chat.actions(), ["ban 30 0"]);

        // Pretend the time is up (as after a restart past the end time).
        s.store
            .schedule_unban(Platform::Fluxer, "g", "30", now() - 1, 1)
            .unwrap();
        s.tools.lift_due_bans(&s.bot).await;
        assert_eq!(s.chat.actions(), ["ban 30 0", "unban 30"]);
        assert!(s.store.due_unbans(now()).unwrap().is_empty());
        let case = s.store.case(Platform::Fluxer, "g", 2).unwrap().unwrap();
        assert_eq!(
            (case.action.as_str(), case.moderator_id.as_str()),
            ("unban", "")
        );
        assert!(s.chat.last_sent("log").contains("botsu (automatic)"));

        // Unbanned by hand before it was due: the schedule is dropped quietly.
        s.say("10", "!mod tempban 30 1d").await;
        s.chat.banned.lock().unwrap().clear();
        s.store
            .schedule_unban(Platform::Fluxer, "g", "30", now() - 1, 3)
            .unwrap();
        s.tools.lift_due_bans(&s.bot).await;
        assert!(s.store.due_unbans(now()).unwrap().is_empty());

        // A permanent ban replaces a temp-ban.
        s.say("10", "!mod tempban 30 1d").await;
        s.say("10", "!mod ban 30").await;
        assert_eq!(
            s.store
                .scheduled_unban(Platform::Fluxer, "g", "30")
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn timeouts() {
        let s = setup();
        assert!(s
            .say("10", "!mod timeout 30 29d")
            .await
            .contains("between 1 minute and 4w"));
        assert!(s
            .say("10", "!mod timeout 30 10m")
            .await
            .contains("Timed out"));
        assert!(s.say("10", "!mod untimeout 30").await.contains("Ended"));
        assert_eq!(s.chat.actions(), ["timeout 30 true", "timeout 30 false"]);
    }

    #[tokio::test]
    async fn grouped_under_mod() {
        let s = setup();
        let member = s.say("30", "!mod").await;
        assert!(member.contains("`!mod warnings`"), "{member}");
        assert!(
            !member.contains("`!mod ban`"),
            "only what they may use: {member}"
        );
        let moderator = s.say("10", "!moderation").await;
        assert!(moderator.contains("`!mod ban`") && moderator.contains("`!mod cleanup`"));

        s.say("30", "!help").await;
        let help = s.chat.all_sent("here");
        assert_eq!(help.matches("\n!mod ").count(), 1, "{help}");
        assert!(
            !help.contains("\n!moderation"),
            "aliases aren't listed: {help}"
        );

        assert!(s
            .say("30", "!mod ban 10")
            .await
            .contains("You need to be a mod to use `!mod ban`"));
        assert!(s.say("10", "!mod nope").await.contains("no `nope`"));
        assert!(s.say("10", "!mod ban").await.contains("Usage: `!mod ban"));
    }

    /// `set autorole` (bot.rs `member_joined`): new members get the role, bots don't, and
    /// without a setting nothing happens.
    #[tokio::test]
    async fn autorole_on_join() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let mut bot = Bot::new("!", Vec::<String>::new());
        bot.enable_server_prefixes(store.clone());
        let chat = FakeChat::default();
        bot.member_joined(&chat, "g", "30", false).await;
        assert!(chat.actions().is_empty());
        store
            .set_guild_setting(
                Platform::Fluxer,
                "g",
                crate::server::AUTOROLE_KEY,
                Some("modr"),
            )
            .unwrap();
        bot.member_joined(&chat, "g", "30", false).await;
        bot.member_joined(&chat, "g", "31", true).await;
        assert_eq!(chat.actions(), vec!["addrole 30 modr"]);
    }
}
