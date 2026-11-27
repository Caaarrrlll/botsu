//! Exporting and importing a server's setup (docs/settings-migration.md): alert subscriptions,
//! admin and mod roles, and the settings in [`SETTINGS`]. Never user data (cases, who added
//! what, scheduled unbans/unmutes). The file is platform-neutral, so it moves between any two
//! servers: Discord ↔ Fluxer, another instance, another botsu install.
//!
//! `admin export` posts the file. `admin import` + the file (server owner) maps channels and
//! roles onto this server by name (or `map <old> = <new>` lines), posts a preview, and holds it
//! for 10 minutes; `admin import confirm` applies it, `admin import cancel` drops it.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::adapter::{ChannelInfo, ChatAdapter, Platform, RoleInfo};
use crate::alerts::{escape_md, parse_channel, BASELINE, EMBEDS_KEY};
use crate::command::{Command, CommandContext};
use crate::error::{Error, Result};
use crate::moderation::{MODLOG_KEY, MOD_DM_KEY, MUTE_ROLE_KEY};
use crate::music::VOLUME_KEY;
use crate::permissions::{Level, ADMIN_ROLE, MOD_ROLE};
use crate::server::{guild, PREFIX_KEY};
use crate::storage::{AlertKind, NewSubscription, Scope, Store, Subscription};

/// The file format this botsu writes and reads.
pub const FORMAT_VERSION: u32 = 1;
const MAX_FILE_BYTES: usize = 1_000_000;
const MAX_SUBSCRIPTIONS: usize = 2000;
/// How long a previewed import waits for `confirm`.
const PENDING_FOR: Duration = Duration::from_secs(10 * 60);
const MAX_MESSAGE: usize = 1900;

// --- what travels ---------------------------------------------------------------------------------

/// How a `guild_settings` value carries over to another server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Portable {
    /// A channel ID: mapped by name.
    Channel,
    /// A role ID: mapped by name.
    Role,
    /// Copied as it is.
    Value,
}

/// Every `guild_settings` key that is exported, and how. A key that isn't listed stays out of
/// exports (a test fails until each `*_KEY` is listed, so new settings get classified).
pub const SETTINGS: &[(&str, Portable)] = &[
    (MODLOG_KEY, Portable::Channel),
    (MUTE_ROLE_KEY, Portable::Role),
    (MOD_DM_KEY, Portable::Value),
    (VOLUME_KEY, Portable::Value),
    (EMBEDS_KEY, Portable::Value),
    (PREFIX_KEY, Portable::Value),
];

/// Short names for the preview.
fn setting_label(key: &str) -> &str {
    match key {
        MODLOG_KEY => "mod log",
        MUTE_ROLE_KEY => "mute role",
        MOD_DM_KEY => "mod DMs",
        VOLUME_KEY => "music volume",
        EMBEDS_KEY => "alert embeds",
        PREFIX_KEY => "command prefix",
        other => other,
    }
}

// --- the file -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    pub botsu_export: u32,
    #[serde(default)]
    pub exported_at: String,
    #[serde(default)]
    pub botsu_version: String,
    pub from: Origin,
    /// Old channel ID → its name.
    #[serde(default)]
    pub channels: BTreeMap<String, Named>,
    /// Old role ID → its name.
    #[serde(default)]
    pub roles: BTreeMap<String, Named>,
    #[serde(default)]
    pub permissions: Permissions,
    /// Registered settings: channel and role values point into `channels` / `roles`.
    #[serde(default)]
    pub settings: BTreeMap<String, SettingValue>,
    #[serde(default)]
    pub subscriptions: Vec<Sub>,
    /// Custom commands (`cc`): name and text, without who made them. Optional in the file, so
    /// exports from before custom commands still import.
    #[serde(default)]
    pub custom_commands: Vec<CustomCom>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCom {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub platform: String,
    pub guild_id: String,
    #[serde(default)]
    pub guild_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Named {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub admin_roles: Vec<String>,
    #[serde(default)]
    pub mod_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Channel { channel: String },
    Role { role: String },
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sub {
    /// `youtube` or `twitch`.
    pub kind: String,
    pub source_id: String,
    pub source_name: String,
    pub source_key: String,
    /// Old channel ID (see `channels`).
    pub channel: String,
    #[serde(default)]
    pub include_shorts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Old role ID, `everyone` or `here` (user mentions aren't exported).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention: Option<String>,
}

fn kind_of(s: &str) -> Option<AlertKind> {
    match s {
        "youtube" => Some(AlertKind::YouTube),
        "twitch" => Some(AlertKind::Twitch),
        _ => None,
    }
}

fn kind_label(k: AlertKind) -> &'static str {
    match k {
        AlertKind::YouTube => "YouTube",
        AlertKind::Twitch => "Twitch",
    }
}

fn platform_name(p: Platform) -> &'static str {
    p.key()
}

/// Reads a file, refusing other formats and oversized ones.
pub fn parse_export(bytes: &[u8]) -> std::result::Result<Export, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("That file is too big for a botsu export (over 1 MB).".into());
    }
    let export: Export = serde_json::from_slice(bytes)
        .map_err(|e| format!("That isn't a botsu export file ({e})."))?;
    if export.botsu_export != FORMAT_VERSION {
        return Err(format!(
            "That file is export format {}; this botsu reads format {FORMAT_VERSION}.",
            export.botsu_export
        ));
    }
    if export.subscriptions.len() > MAX_SUBSCRIPTIONS {
        return Err(format!(
            "That file has more than {MAX_SUBSCRIPTIONS} alerts."
        ));
    }
    Ok(export)
}

// --- export ---------------------------------------------------------------------------------------

/// Everything this server has set up, with the channel and role names needed to map it.
pub async fn build_export(
    store: &Store,
    chat: &dyn ChatAdapter,
    platform: Platform,
    guild_id: &str,
) -> Result<Export> {
    let channel_names: HashMap<String, String> = chat
        .text_channels(guild_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.id, c.name))
        .collect();
    let role_names: HashMap<String, String> = chat
        .roles(guild_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();
    let mut channels = BTreeMap::new();
    let mut roles = BTreeMap::new();
    let mut note_channel = |id: &str| {
        let name = channel_names.get(id).cloned().unwrap_or_default();
        channels.insert(id.to_owned(), Named { name });
    };
    let note_role = |id: &str, roles: &mut BTreeMap<String, Named>| {
        let name = role_names.get(id).cloned().unwrap_or_default();
        roles.insert(id.to_owned(), Named { name });
    };

    let scope = Scope::Guild(guild_id);
    let mut subscriptions = Vec::new();
    for kind in [AlertKind::YouTube, AlertKind::Twitch] {
        for s in store.list_subscriptions(kind, platform, scope)? {
            note_channel(&s.channel_id);
            // User mentions are about people: left out.
            let mention = s.mention.filter(|m| !m.starts_with("user:"));
            if let Some(m) = mention
                .as_deref()
                .filter(|m| !matches!(*m, "everyone" | "here"))
            {
                note_role(m, &mut roles);
            }
            subscriptions.push(Sub {
                kind: match kind {
                    AlertKind::YouTube => "youtube",
                    AlertKind::Twitch => "twitch",
                }
                .to_owned(),
                source_id: s.source_id,
                source_name: s.source_name,
                source_key: s.source_key,
                channel: s.channel_id,
                include_shorts: s.include_shorts,
                message: s.message,
                mention,
            });
        }
    }

    let mut permissions = Permissions::default();
    for (level, role) in store.guild_roles(platform, guild_id)? {
        note_role(&role, &mut roles);
        if level == ADMIN_ROLE {
            permissions.admin_roles.push(role);
        } else if level == MOD_ROLE {
            permissions.mod_roles.push(role);
        }
    }

    let mut settings = BTreeMap::new();
    for (key, how) in SETTINGS {
        let Some(value) = store.guild_setting(platform, guild_id, key)? else {
            continue;
        };
        let v = match how {
            Portable::Channel => {
                note_channel(&value);
                SettingValue::Channel { channel: value }
            }
            Portable::Role => {
                note_role(&value, &mut roles);
                SettingValue::Role { role: value }
            }
            Portable::Value => SettingValue::Value(value),
        };
        settings.insert((*key).to_owned(), v);
    }

    Ok(Export {
        botsu_export: FORMAT_VERSION,
        exported_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default(),
        botsu_version: env!("CARGO_PKG_VERSION").to_owned(),
        from: Origin {
            platform: platform_name(platform).to_owned(),
            guild_id: guild_id.to_owned(),
            guild_name: chat.guild_name(guild_id).await.unwrap_or_default(),
        },
        channels,
        roles,
        permissions,
        settings,
        subscriptions,
        custom_commands: store
            .custom_commands(platform, guild_id)?
            .into_iter()
            .map(|(name, text)| CustomCom { name, text })
            .collect(),
    })
}

// --- mapping onto this server ---------------------------------------------------------------------

/// Channel names compare without case, a leading `#`, spaces, `-` and `_`: Discord's
/// `youtube-alerts` matches Fluxer's "YouTube Alerts".
pub fn norm_channel(name: &str) -> String {
    name.trim()
        .trim_start_matches('#')
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// `map <old name or ID> = <new mention, ID or name>` lines from the import message.
pub fn parse_overrides(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("map ")?;
            let (old, new) = rest.split_once('=')?;
            let (old, new) = (old.trim(), new.trim());
            (!old.is_empty() && !new.is_empty()).then(|| (old.to_owned(), new.to_owned()))
        })
        .collect()
}

/// What this server has, for mapping.
pub struct TargetServer {
    pub guild_id: String,
    pub channels: Vec<ChannelInfo>,
    pub roles: Vec<RoleInfo>,
    /// Names of the custom commands it already has.
    pub custom_commands: Vec<String>,
}

impl TargetServer {
    fn channel_by_arg(&self, arg: &str) -> Option<&ChannelInfo> {
        match parse_channel(arg) {
            Some(id) => self.channels.iter().find(|c| c.id == id),
            None => {
                let n = norm_channel(arg);
                self.channels.iter().find(|c| norm_channel(&c.name) == n)
            }
        }
    }

    fn role_by_arg(&self, arg: &str) -> Option<&RoleInfo> {
        let arg = arg.trim();
        let id = arg
            .strip_prefix("<@&")
            .and_then(|r| r.strip_suffix('>'))
            .unwrap_or(arg);
        let name = arg.trim_start_matches('@');
        self.roles
            .iter()
            .filter(|r| r.id != self.guild_id) // never @everyone
            .find(|r| r.id == id)
            .or_else(|| {
                self.roles
                    .iter()
                    .filter(|r| r.id != self.guild_id)
                    .find(|r| r.name.eq_ignore_ascii_case(name))
            })
    }
}

/// Maps the export's channels and roles onto `target`: overrides first (old side by ID or
/// name), then by name.
struct Mapper<'a> {
    export: &'a Export,
    target: &'a TargetServer,
    overrides: &'a [(String, String)],
}

impl Mapper<'_> {
    fn old_matches(old_id: &str, old_name: &str, given: &str, channel: bool) -> bool {
        if given == old_id {
            return true;
        }
        if channel {
            norm_channel(given) == norm_channel(old_name)
        } else {
            given.trim_start_matches('@').eq_ignore_ascii_case(old_name)
        }
    }

    fn channel(&self, old_id: &str) -> Option<&ChannelInfo> {
        let old_name = self
            .export
            .channels
            .get(old_id)
            .map(|n| n.name.as_str())
            .unwrap_or("");
        for (old, new) in self.overrides {
            if Self::old_matches(old_id, old_name, old, true) {
                if let Some(c) = self.target.channel_by_arg(new) {
                    return Some(c);
                }
            }
        }
        if old_name.is_empty() {
            return None;
        }
        self.target.channel_by_arg(old_name)
    }

    fn role(&self, old_id: &str) -> Option<&RoleInfo> {
        let old_name = self
            .export
            .roles
            .get(old_id)
            .map(|n| n.name.as_str())
            .unwrap_or("");
        for (old, new) in self.overrides {
            if Self::old_matches(old_id, old_name, old, false) {
                if let Some(r) = self.target.role_by_arg(new) {
                    return Some(r);
                }
            }
        }
        if old_name.is_empty() {
            return None;
        }
        self.target
            .roles
            .iter()
            .filter(|r| r.id != self.target.guild_id)
            .find(|r| r.name.eq_ignore_ascii_case(old_name))
    }

    fn channel_label(&self, old_id: &str) -> String {
        match self.export.channels.get(old_id) {
            Some(n) if !n.name.is_empty() => format!("#{}", n.name),
            _ => format!("channel `{old_id}`"),
        }
    }

    fn role_label(&self, old_id: &str) -> String {
        match self.export.roles.get(old_id) {
            Some(n) if !n.name.is_empty() => format!("`@{}`", n.name),
            _ => format!("role `{old_id}`"),
        }
    }
}

/// One subscription to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSub {
    pub kind: AlertKind,
    pub source_id: String,
    pub source_name: String,
    pub source_key: String,
    pub channel: String,
    pub include_shorts: bool,
    pub message: Option<String>,
    pub mention: Option<String>,
}

/// What an import will do; built without touching anything, applied on `confirm`.
#[derive(Debug, Clone)]
pub struct Plan {
    pub replace: bool,
    pub subs: Vec<PlannedSub>,
    pub roles: Vec<(&'static str, String)>,
    pub settings: Vec<(String, String)>,
    /// Custom commands to add or replace, as `(name, text)`.
    pub custom_commands: Vec<(String, String)>,
    /// One line per item, for the preview.
    pub lines: Vec<String>,
    pub from: String,
}

/// Builds the plan. `existing` is this server's current subscriptions (for "already there" when
/// merging); `twitch_ready` says whether this bot can poll Twitch.
pub fn plan(
    export: &Export,
    target: &TargetServer,
    overrides: &[(String, String)],
    replace: bool,
    existing: &[Subscription],
    twitch_ready: bool,
) -> Plan {
    let m = Mapper {
        export,
        target,
        overrides,
    };
    let mut lines = Vec::new();
    let mut subs = Vec::new();

    for s in &export.subscriptions {
        let Some(kind) = kind_of(&s.kind) else {
            lines.push(format!(
                "• `{}` alert {}: unknown kind, skipped",
                s.kind, s.source_name
            ));
            continue;
        };
        let what = format!("{} **{}**", kind_label(kind), escape_md(&s.source_name));
        let Some(channel) = m.channel(&s.channel) else {
            lines.push(format!(
                "• {what}: {} not found here, skipped",
                m.channel_label(&s.channel)
            ));
            continue;
        };
        let already = !replace
            && existing.iter().any(|e| {
                e.kind == kind && e.source_id == s.source_id && e.channel_id == channel.id
            });
        if already {
            lines.push(format!("• {what} → <#{}>: already there", channel.id));
            continue;
        }
        let mut notes = Vec::new();
        let mention = match s.mention.as_deref() {
            None => None,
            Some(m @ ("everyone" | "here")) => Some(m.to_owned()),
            Some(old) => match m.role(old) {
                Some(r) => Some(r.id.clone()),
                None => {
                    notes.push(format!("mention {} not found, left out", m.role_label(old)));
                    None
                }
            },
        };
        if kind == AlertKind::Twitch && !twitch_ready {
            notes.push("Twitch isn't set up on this bot, so it won't post yet".into());
        }
        if s.message.is_some() {
            notes.push("custom text".into());
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join("; "))
        };
        lines.push(format!("• {what} → <#{}>: add{notes}", channel.id));
        subs.push(PlannedSub {
            kind,
            source_id: s.source_id.clone(),
            source_name: s.source_name.clone(),
            source_key: s.source_key.clone(),
            channel: channel.id.clone(),
            include_shorts: s.include_shorts,
            message: s.message.clone(),
            mention,
        });
    }

    let mut roles = Vec::new();
    for (level, list) in [
        (ADMIN_ROLE, &export.permissions.admin_roles),
        (MOD_ROLE, &export.permissions.mod_roles),
    ] {
        for old in list {
            match m.role(old) {
                Some(r) => {
                    lines.push(format!(
                        "• {level} role {} → **{}**",
                        m.role_label(old),
                        escape_md(&r.name)
                    ));
                    roles.push((level, r.id.clone()));
                }
                None => lines.push(format!(
                    "• {level} role {}: not found here, skipped",
                    m.role_label(old)
                )),
            }
        }
    }

    let mut settings = Vec::new();
    for (key, how) in SETTINGS {
        let Some(value) = export.settings.get(*key) else {
            continue;
        };
        let label = setting_label(key);
        let mapped = match (how, value) {
            (Portable::Channel, SettingValue::Channel { channel }) => match m.channel(channel) {
                Some(c) => {
                    lines.push(format!("• {label} → <#{}>", c.id));
                    Some(c.id.clone())
                }
                None => {
                    lines.push(format!(
                        "• {label}: {} not found here, skipped",
                        m.channel_label(channel)
                    ));
                    None
                }
            },
            (Portable::Role, SettingValue::Role { role }) => match m.role(role) {
                Some(r) => {
                    lines.push(format!("• {label} → **{}**", escape_md(&r.name)));
                    Some(r.id.clone())
                }
                None => {
                    lines.push(format!(
                        "• {label}: {} not found here, skipped",
                        m.role_label(role)
                    ));
                    None
                }
            },
            (Portable::Value, SettingValue::Value(v)) => {
                lines.push(format!("• {label}: {v}"));
                Some(v.clone())
            }
            _ => {
                lines.push(format!("• {label}: unreadable in the file, skipped"));
                None
            }
        };
        if let Some(v) = mapped {
            settings.push(((*key).to_owned(), v));
        }
    }

    let mut custom_commands = Vec::new();
    for c in &export.custom_commands {
        if !crate::custom::valid_name(&c.name) || c.text.trim().is_empty() {
            lines.push(format!(
                "• custom command `{}`: unusable name or text, skipped",
                c.name
            ));
        } else if !replace && target.custom_commands.contains(&c.name) {
            lines.push(format!(
                "• custom command `{}`: already there, kept",
                c.name
            ));
        } else {
            lines.push(format!("• custom command `{}`: add", c.name));
            custom_commands.push((c.name.clone(), c.text.clone()));
        }
    }

    let name = if export.from.guild_name.is_empty() {
        format!("`{}`", export.from.guild_id)
    } else {
        format!("**{}**", escape_md(&export.from.guild_name))
    };
    let platform = Platform::from_key(&export.from.platform)
        .map(|p| p.to_string())
        .unwrap_or_else(|| export.from.platform.clone());
    Plan {
        replace,
        subs,
        roles,
        settings,
        custom_commands,
        lines,
        from: format!("{platform} server {name}"),
    }
}

impl Plan {
    pub fn preview(&self, prefix: &str) -> String {
        let mut out = format!("**Import preview** from {}\n", self.from);
        if self.replace {
            out.push_str(
                "**Replace:** this server's alerts, admin/mod roles and settings are removed \
                 first.\n",
            );
        }
        if self.lines.is_empty() {
            out.push_str("The file has nothing to import.\n");
        } else {
            out.push_str(&self.lines.join("\n"));
            out.push('\n');
        }
        out.push_str(&format!(
            "`{prefix}import confirm` within 10 minutes applies this; \
             `{prefix}import cancel` drops it."
        ));
        out
    }
}

/// Whether any channel (any platform) follows the source already; then the poller owns its
/// seen-items record and nothing may be marked here.
fn is_followed(store: &Store, kind: AlertKind, source_id: &str) -> Result<bool> {
    Ok(store
        .all_subscriptions(kind)?
        .iter()
        .any(|s| s.source_id == source_id))
}

/// Applies a plan. Roles and settings go in one transaction; subscriptions one by one, after a
/// notice in each target channel (which also proves the bot can post there).
pub async fn apply(
    plan: &Plan,
    store: &Store,
    chat: &dyn ChatAdapter,
    platform: Platform,
    guild_id: &str,
    user_id: &str,
) -> Result<String> {
    let keys: Vec<&str> = SETTINGS.iter().map(|(k, _)| *k).collect();
    let removed = if plan.replace {
        store.clear_server_config(platform, guild_id, &keys)?
    } else {
        0
    };
    store.apply_server_config(platform, guild_id, &plan.roles, &plan.settings)?;
    for (name, text) in &plan.custom_commands {
        store.set_custom_command(platform, guild_id, name, text, user_id)?;
    }

    let mut channels: Vec<&str> = plan.subs.iter().map(|s| s.channel.as_str()).collect();
    channels.sort_unstable();
    channels.dedup();
    let mut unusable = Vec::new();
    for c in channels {
        let notice = format!("Alerts imported from {} will post here.", plan.from);
        if chat.send_message(c, &notice).await.is_err() {
            unusable.push(c.to_owned());
        }
    }

    let (mut added, mut failed) = (0, Vec::new());
    for s in &plan.subs {
        if unusable.contains(&s.channel) {
            failed.push(format!(
                "{} (can't post in <#{}>)",
                escape_md(&s.source_name),
                s.channel
            ));
            continue;
        }
        // A new Twitch source: everything from now on is new. (A new YouTube source needs no
        // marker: the poller's first look at its feed marks what's there without posting.)
        if s.kind == AlertKind::Twitch && !is_followed(store, s.kind, &s.source_id)? {
            store.mark_seen(s.kind, &s.source_id, BASELINE)?;
        }
        let new = store.add_subscription(&NewSubscription {
            kind: s.kind,
            source_id: &s.source_id,
            source_name: &s.source_name,
            source_key: &s.source_key,
            platform,
            guild_id: Some(guild_id),
            channel_id: &s.channel,
            created_by: user_id,
        })?;
        if !new {
            continue;
        }
        if let Some(id) = store.subscription_id(s.kind, &s.source_id, platform, &s.channel)? {
            let ids = [id];
            if s.include_shorts {
                store.set_include_shorts(&ids, true)?;
            }
            store.set_alert_message(&ids, s.message.as_deref())?;
            store.set_alert_mention(&ids, s.mention.as_deref())?;
        }
        added += 1;
    }

    let mut out = format!(
        "Imported from {}: {added} alert(s), {} role(s), {} setting(s), {} custom command(s).",
        plan.from,
        plan.roles.len(),
        plan.settings.len(),
        plan.custom_commands.len()
    );
    if plan.replace {
        out.push_str(&format!(" ({removed} old alert(s) removed first.)"));
    }
    if !failed.is_empty() {
        out.push_str(&format!("\nNot added: {}.", failed.join(", ")));
    }
    Ok(out)
}

// --- commands -------------------------------------------------------------------------------------

/// `admin export`: posts this server's setup as a file (admins).
pub struct ExportCommand {
    store: std::sync::Arc<Store>,
}

impl ExportCommand {
    pub fn new(store: std::sync::Arc<Store>) -> Self {
        Self { store }
    }
}

/// `botsu-discord-my-server-2026-11-20.json`.
fn file_name(export: &Export) -> String {
    let slug: String = export
        .from
        .guild_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        export.from.guild_id.clone()
    } else {
        slug
    };
    let date = export.exported_at.get(..10).unwrap_or("export");
    format!("botsu-{}-{slug}-{date}.json", export.from.platform)
}

#[async_trait]
impl Command for ExportCommand {
    fn name(&self) -> &'static str {
        "export"
    }

    fn help(&self) -> &'static str {
        "Save this server's alerts, roles and settings as a file, to import elsewhere (admins)."
    }

    fn level(&self) -> Level {
        Level::Admin
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let export = build_export(&self.store, &*ctx.chat, ctx.message.platform, &guild).await?;
        let json = serde_json::to_vec_pretty(&export)
            .map_err(|e| Error::internal("writing the export", e))?;
        let text = format!(
            "This server's setup: {} alert(s), {} admin/mod role(s), {} setting(s). No user \
             data. Import it with `{}import` and this file attached.",
            export.subscriptions.len(),
            export.permissions.admin_roles.len() + export.permissions.mod_roles.len(),
            export.settings.len(),
            ctx.prefix
        );
        ctx.reply_dm(&text, Some((&file_name(&export), json)))
            .await?;
        Ok(())
    }
}

type PendingKey = (Platform, String, String);

/// `admin import [--replace] [map <old> = <new> …]` + the file; `confirm`; `cancel`
/// (server owner).
pub struct ImportCommand {
    store: std::sync::Arc<Store>,
    http: reqwest::Client,
    twitch_ready: bool,
    pending: Mutex<HashMap<PendingKey, (Instant, Plan)>>,
}

impl ImportCommand {
    pub fn new(store: std::sync::Arc<Store>, http: reqwest::Client, twitch_ready: bool) -> Self {
        Self {
            store,
            http,
            twitch_ready,
            pending: Mutex::default(),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<PendingKey, (Instant, Plan)>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn usage(p: &str) -> String {
        format!(
            "Usage: `{p}import` with a botsu export file attached (from `{p}export`). \
             Options on the same message: `--replace` (remove this server's alerts, roles and \
             settings first), and lines like `map old-channel = #channel` or \
             `map Old Role = @Role` for names that don't match. Then `{p}import confirm` \
             or `{p}import cancel`."
        )
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| Error::Usage(format!("I couldn't download the file: {e}")))?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Usage(format!("I couldn't download the file: {e}")))?;
        Ok(bytes.to_vec())
    }

    async fn preview(&self, ctx: &CommandContext, guild: &str) -> Result<()> {
        let Some(file) = ctx.message.attachments.first() else {
            return Err(Error::Usage(Self::usage(&ctx.prefix)));
        };
        if file.size as usize > MAX_FILE_BYTES {
            return Err(Error::Usage(
                "That file is too big for a botsu export (over 1 MB).".into(),
            ));
        }
        let bytes = self.download(&file.url).await?;
        let export = parse_export(&bytes).map_err(Error::Usage)?;

        let platform = ctx.message.platform;
        let target = TargetServer {
            guild_id: guild.to_owned(),
            channels: ctx.chat.text_channels(guild).await?,
            roles: ctx.chat.roles(guild).await?,
            custom_commands: self
                .store
                .custom_commands(ctx.message.platform, guild)?
                .into_iter()
                .map(|(name, _)| name)
                .collect(),
        };
        let scope = Scope::Guild(guild);
        let mut existing = self
            .store
            .list_subscriptions(AlertKind::YouTube, platform, scope)?;
        existing.extend(
            self.store
                .list_subscriptions(AlertKind::Twitch, platform, scope)?,
        );
        let replace = ctx.args.split_whitespace().any(|w| w == "--replace");
        let overrides = parse_overrides(&ctx.args);
        let plan = plan(
            &export,
            &target,
            &overrides,
            replace,
            &existing,
            self.twitch_ready,
        );
        let text = plan.preview(&ctx.prefix);
        self.pending().insert(
            (platform, guild.to_owned(), ctx.message.author.id.clone()),
            (Instant::now(), plan),
        );
        if text.chars().count() <= MAX_MESSAGE {
            ctx.reply_dm(&text, None).await?;
        } else {
            let note = format!(
                "The preview is long, so it's in the file. `{p}import confirm` within 10                  minutes applies it; `{p}import cancel` drops it.",
                p = ctx.prefix
            );
            ctx.reply_dm(&note, Some(("import-preview.txt", text.into_bytes())))
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Command for ImportCommand {
    fn name(&self) -> &'static str {
        "import"
    }

    fn help(&self) -> &'static str {
        "Import alerts, roles and settings from an `export` file: preview, then confirm (server owner)."
    }

    fn level(&self) -> Level {
        Level::ServerOwner
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        let guild = guild(&ctx)?;
        let key = (
            ctx.message.platform,
            guild.clone(),
            ctx.message.author.id.clone(),
        );
        match ctx
            .args
            .split_whitespace()
            .next()
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("confirm") => {
                let pending = self.pending().remove(&key);
                let Some((at, plan)) = pending.filter(|(at, _)| at.elapsed() < PENDING_FOR) else {
                    return Err(Error::Usage(format!(
                        "Nothing to confirm: run `{}import` with the file first (a preview \
                         is kept for 10 minutes).",
                        ctx.prefix
                    )));
                };
                let _ = at;
                let summary = apply(
                    &plan,
                    &self.store,
                    &*ctx.chat,
                    ctx.message.platform,
                    &guild,
                    &ctx.message.author.id,
                )
                .await?;
                ctx.reply_dm(&summary, None).await?;
                Ok(())
            }
            Some("cancel") => {
                let had = self.pending().remove(&key).is_some();
                ctx.reply(if had {
                    "Import dropped; nothing was changed."
                } else {
                    "There was no import waiting."
                })
                .await?;
                Ok(())
            }
            _ => self.preview(&ctx, &guild).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(n: &str) -> Named {
        Named { name: n.into() }
    }

    fn sample() -> Export {
        Export {
            botsu_export: FORMAT_VERSION,
            exported_at: "2026-11-20T12:00:00Z".into(),
            botsu_version: "0.1.0".into(),
            from: Origin {
                platform: "discord".into(),
                guild_id: "1".into(),
                guild_name: "Old Server".into(),
            },
            channels: BTreeMap::from([
                ("11".into(), named("youtube-alerts")),
                ("12".into(), named("mod-log")),
                ("13".into(), named("gone")),
            ]),
            roles: BTreeMap::from([
                ("21".into(), named("Admins")),
                ("22".into(), named("Mods")),
                ("23".into(), named("Pingers")),
            ]),
            permissions: Permissions {
                admin_roles: vec!["21".into()],
                mod_roles: vec!["22".into()],
            },
            settings: BTreeMap::from([
                (
                    MODLOG_KEY.into(),
                    SettingValue::Channel {
                        channel: "12".into(),
                    },
                ),
                (VOLUME_KEY.into(), SettingValue::Value("80".into())),
            ]),
            subscriptions: vec![
                Sub {
                    kind: "youtube".into(),
                    source_id: "UC1".into(),
                    source_name: "Chan_One".into(),
                    source_key: "UC1".into(),
                    channel: "11".into(),
                    include_shorts: true,
                    message: Some("{name}!".into()),
                    mention: Some("23".into()),
                },
                Sub {
                    kind: "twitch".into(),
                    source_id: "99".into(),
                    source_name: "streamer".into(),
                    source_key: "streamer".into(),
                    channel: "13".into(),
                    include_shorts: false,
                    message: None,
                    mention: None,
                },
            ],
            custom_commands: vec![
                CustomCom {
                    name: "rules".into(),
                    text: "Be nice, {author}.".into(),
                },
                CustomCom {
                    name: "ip".into(),
                    text: "play.example.org".into(),
                },
            ],
        }
    }

    fn role(id: &str, name: &str) -> RoleInfo {
        RoleInfo {
            id: id.into(),
            name: name.into(),
            permissions: 0,
            position: 1,
        }
    }

    fn target() -> TargetServer {
        TargetServer {
            guild_id: "900".into(),
            channels: vec![
                ChannelInfo {
                    id: "911".into(),
                    name: "YouTube Alerts".into(),
                },
                ChannelInfo {
                    id: "912".into(),
                    name: "moderation".into(),
                },
            ],
            roles: vec![
                role("900", "@everyone"),
                role("921", "admins"),
                role("922", "Moderators"),
            ],
            custom_commands: vec!["rules".into()],
        }
    }

    #[test]
    fn file_round_trip() {
        let e = sample();
        let json = serde_json::to_vec_pretty(&e).unwrap();
        assert_eq!(parse_export(&json).unwrap(), e);
        let mut newer = e.clone();
        newer.botsu_export = 2;
        let err = parse_export(&serde_json::to_vec(&newer).unwrap()).unwrap_err();
        assert!(err.contains("format 2"));
        assert!(parse_export(b"{\"hello\": 1}").is_err());
    }

    #[test]
    fn channel_names_match_across_platforms() {
        assert_eq!(
            norm_channel("#youtube-alerts"),
            norm_channel("YouTube Alerts")
        );
        assert_eq!(norm_channel("mod_log"), "modlog");
    }

    #[test]
    fn overrides() {
        assert_eq!(
            parse_overrides("--replace\nmap mod-log = #moderation\nmap Mods = <@&922>\nmap x ="),
            vec![
                ("mod-log".to_owned(), "#moderation".to_owned()),
                ("Mods".to_owned(), "<@&922>".to_owned())
            ]
        );
    }

    #[test]
    fn plan_maps_by_name_and_override() {
        let e = sample();
        let t = target();
        let o = parse_overrides("map mod-log = moderation\nmap @Mods = @Moderators");
        let p = plan(&e, &t, &o, false, &[], true);
        // YouTube: channel by name across platforms; the mention role doesn't exist here.
        assert_eq!(p.subs.len(), 1);
        assert_eq!(p.subs[0].channel, "911");
        assert_eq!(p.subs[0].mention, None);
        assert!(p.subs[0].include_shorts);
        // Twitch: its channel is gone.
        assert!(p.lines.iter().any(|l| l.contains("#gone not found")));
        // Roles: Admins by name (case), Mods via the override; never @everyone.
        assert_eq!(
            p.roles,
            vec![(ADMIN_ROLE, "921".to_owned()), (MOD_ROLE, "922".to_owned())]
        );
        // Settings: mod log via the override, volume copied.
        assert!(p
            .settings
            .contains(&(MODLOG_KEY.to_owned(), "912".to_owned())));
        assert!(p
            .settings
            .contains(&(VOLUME_KEY.to_owned(), "80".to_owned())));
        assert!(p.preview("!admin ").contains("`!admin import confirm`"));
        // Custom commands: "rules" exists here already, "ip" is new.
        assert_eq!(
            p.custom_commands,
            vec![("ip".to_owned(), "play.example.org".to_owned())]
        );
        assert!(p.lines.iter().any(|l| l.contains("`rules`: already there")));
    }

    #[test]
    fn existing_alerts_are_kept_when_merging() {
        let store = Store::open_in_memory().unwrap();
        store
            .add_subscription(&NewSubscription {
                kind: AlertKind::YouTube,
                source_id: "UC1",
                source_name: "Chan_One",
                source_key: "UC1",
                platform: Platform::Fluxer,
                guild_id: Some("900"),
                channel_id: "911",
                created_by: "u",
            })
            .unwrap();
        let existing = store
            .list_subscriptions(AlertKind::YouTube, Platform::Fluxer, Scope::Guild("900"))
            .unwrap();
        let p = plan(&sample(), &target(), &[], false, &existing, true);
        assert!(p.subs.is_empty());
        assert!(p.lines.iter().any(|l| l.contains("already there")));
        let p = plan(&sample(), &target(), &[], true, &existing, true);
        assert_eq!(p.subs.len(), 1, "replace re-adds it");
    }

    /// Accepts every message; nothing else.
    struct Quiet;

    #[async_trait]
    impl ChatAdapter for Quiet {
        fn platform(&self) -> Platform {
            Platform::Fluxer
        }
        fn capabilities(&self) -> crate::adapter::Capabilities {
            crate::adapter::Capabilities {
                bulk_delete: false,
                slash_commands: false,
                voice: false,
            }
        }
        async fn send_message(&self, _: &str, _: &str) -> Result<String> {
            Ok("m".into())
        }
    }

    #[tokio::test]
    async fn apply_writes_everything() {
        let store = Store::open_in_memory().unwrap();
        let o = parse_overrides(
            "map Pingers = Moderators
map mod-log = moderation
map Mods = Moderators",
        );
        let p = plan(&sample(), &target(), &o, false, &[], true);
        let summary = apply(&p, &store, &Quiet, Platform::Fluxer, "900", "owner")
            .await
            .unwrap();
        // "rules" exists on the target already (merge keeps it); "ip" is added.
        assert!(
            summary.contains("1 alert(s), 2 role(s), 2 setting(s), 1 custom command(s)"),
            "{summary}"
        );
        assert_eq!(
            store
                .custom_command(Platform::Fluxer, "900", "ip")
                .unwrap()
                .as_deref(),
            Some("play.example.org")
        );
        let subs = store
            .list_subscriptions(AlertKind::YouTube, Platform::Fluxer, Scope::Guild("900"))
            .unwrap();
        assert_eq!(subs.len(), 1);
        let s = &subs[0];
        assert_eq!(s.channel_id, "911");
        assert!(s.include_shorts);
        assert_eq!(s.message.as_deref(), Some("{name}!"));
        assert_eq!(s.mention.as_deref(), Some("922"));
        assert_eq!(s.created_by, "owner");
        assert_eq!(store.guild_roles(Platform::Fluxer, "900").unwrap().len(), 2);
        assert_eq!(
            store
                .guild_setting(Platform::Fluxer, "900", VOLUME_KEY)
                .unwrap()
                .as_deref(),
            Some("80")
        );

        // --replace clears first: an alert that isn't in the file disappears.
        store
            .add_subscription(&NewSubscription {
                kind: AlertKind::YouTube,
                source_id: "UC_OTHER",
                source_name: "Other",
                source_key: "UC_OTHER",
                platform: Platform::Fluxer,
                guild_id: Some("900"),
                channel_id: "911",
                created_by: "u",
            })
            .unwrap();
        let p = plan(&sample(), &target(), &o, true, &[], true);
        apply(&p, &store, &Quiet, Platform::Fluxer, "900", "owner")
            .await
            .unwrap();
        let subs = store
            .list_subscriptions(AlertKind::YouTube, Platform::Fluxer, Scope::Guild("900"))
            .unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].source_id, "UC1");
    }

    #[test]
    fn every_setting_key_is_classified() {
        let sources = [
            include_str!("moderation.rs"),
            include_str!("music.rs"),
            include_str!("alerts/mod.rs"),
            include_str!("server.rs"),
        ];
        let listed: Vec<&str> = SETTINGS.iter().map(|(k, _)| *k).collect();
        for src in sources {
            for line in src.lines() {
                let line = line.trim();
                if !(line.starts_with("pub const ") && line.contains("_KEY: &str = \"")) {
                    continue;
                }
                let value = line.split('"').nth(1).unwrap();
                assert!(
                    listed.contains(&value),
                    "guild_settings key {value:?} isn't in transfer::SETTINGS"
                );
            }
        }
    }
}
