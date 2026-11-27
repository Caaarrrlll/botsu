//! Copying a server's structure to another server: roles (name, colour, permissions, hoist,
//! mentionable, order), categories and channels (text, announcement, voice; name, topic, order,
//! category, slowmode, NSFW, bitrate, user limit), each channel's per-role permission overrides,
//! and the server's name, icon and banner. Nothing about members: no member roles, no per-user
//! overrides, no messages.
//!
//! `admin structure export` (admins) DMs the file to whoever asked. `admin structure import` +
//! the file (server owner) DMs a preview; `admin structure confirm` builds it, `cancel` drops it.
//! Reading is supported on Discord, building on Fluxer (the adapters say what they can do).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::adapter::{ChatAdapter, Platform};
use crate::command::{Command, CommandContext};
use crate::error::{Error, Result};
use crate::permissions::Level;
use crate::server::guild;

pub const FORMAT_VERSION: u32 = 1;
const MAX_FILE_BYTES: usize = 2_000_000;
const PENDING_FOR: Duration = Duration::from_secs(10 * 60);

/// Permission bits Fluxer defines (its docs list them; it drops any other bit). Discord uses
/// the same values for all of these.
pub const FLUXER_PERMISSIONS: u64 = {
    let mut bits = 0u64;
    let defined: [u32; 41] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 21, 22, 23, 24, 25,
        26, 27, 28, 29, 30, 34, 35, 36, 37, 38, 40, 43, 51, 52, 53, 54,
    ];
    let mut i = 0;
    while i < defined.len() {
        bits |= 1 << defined[i];
        i += 1;
    }
    bits
};

// --- the file -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Structure {
    pub botsu_structure: u32,
    #[serde(default)]
    pub exported_at: String,
    pub from_platform: String,
    pub name: String,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub banner_url: Option<String>,
    /// The @everyone role's permissions (bits, decimal string like the platforms send them).
    pub everyone_permissions: String,
    /// The old @everyone role ID (the server's ID), as overrides refer to it.
    pub everyone_id: String,
    /// Highest first. Without @everyone and roles that belong to bots or integrations.
    pub roles: Vec<RoleSpec>,
    /// In the order they should be created: loose channels, then each category followed by its
    /// channels (text before voice).
    pub channels: Vec<ChannelSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSpec {
    pub id: String,
    pub name: String,
    pub color: u32,
    pub permissions: String,
    pub hoist: bool,
    pub mentionable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Category,
    Text,
    Announcement,
    Voice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSpec {
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    #[serde(default)]
    pub topic: Option<String>,
    /// Old ID of its category.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub nsfw: bool,
    /// Seconds between messages per member (0 = off).
    #[serde(default)]
    pub slowmode: u32,
    #[serde(default)]
    pub bitrate: Option<u32>,
    #[serde(default)]
    pub user_limit: Option<u32>,
    /// Per-role overrides (old role IDs; the @everyone override uses `everyone_id`).
    #[serde(default)]
    pub overwrites: Vec<Overwrite>,
    /// How the source platform's channel type was changed to fit (for the preview), if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converted_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overwrite {
    pub role: String,
    pub allow: String,
    pub deny: String,
}

pub fn parse(bytes: &[u8]) -> std::result::Result<Structure, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("That file is too big for a botsu structure file.".into());
    }
    let s: Structure = serde_json::from_slice(bytes)
        .map_err(|e| format!("That isn't a botsu structure file ({e})."))?;
    if s.botsu_structure != FORMAT_VERSION {
        return Err(format!(
            "That file is structure format {}; this botsu reads format {FORMAT_VERSION}.",
            s.botsu_structure
        ));
    }
    Ok(s)
}

/// Keeps only the bits Fluxer defines, as a decimal string.
fn fluxer_bits(perms: &str) -> String {
    (perms.parse::<u64>().unwrap_or(0) & FLUXER_PERMISSIONS).to_string()
}

/// `botsu-structure-<server>-<date>.json`.
pub fn file_name(s: &Structure) -> String {
    let slug: String = s
        .name
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
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let date = s.exported_at.get(..10).unwrap_or("export");
    format!(
        "botsu-structure-{}-{date}.json",
        if slug.is_empty() { "server" } else { &slug }
    )
}

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

// --- building it ---------------------------------------------------------------------------------

/// A summary for the preview.
pub fn preview(
    s: &Structure,
    prefix: &str,
    existing_roles: usize,
    existing_channels: usize,
) -> String {
    let count = |k: ChannelKind| s.channels.iter().filter(|c| c.kind == k).count();
    let converted: Vec<String> = s
        .channels
        .iter()
        .filter_map(|c| {
            c.converted_from
                .as_ref()
                .map(|f| format!("#{} ({f})", c.name))
        })
        .collect();
    let mut out = format!(
        "**Structure preview** from {} server **{}**\n\
         • {} roles (plus @everyone's permissions)\n\
         • {} categories, {} text, {} announcement, {} voice channels, with their role overrides\n\
         • server name, icon{}\n",
        s.from_platform,
        s.name,
        s.roles.len(),
        count(ChannelKind::Category),
        count(ChannelKind::Text),
        count(ChannelKind::Announcement),
        count(ChannelKind::Voice),
        if s.banner_url.is_some() {
            ", banner"
        } else {
            ""
        },
    );
    if !converted.is_empty() {
        out.push_str(&format!("• changed to fit: {}\n", converted.join(", ")));
    }
    out.push_str(&format!(
        "This server already has {existing_roles} role(s) and {existing_channels} channel(s); \
         they stay as they are and the new ones are added.\n\
         Fluxer creates about 10 roles or channels a minute, so this takes about {} minute(s). \
         I'll message you when it's done.\n\
         `{prefix}structure confirm` within 10 minutes builds it; \
         `{prefix}structure cancel` drops it.",
        (s.roles.len() + s.channels.len()).div_ceil(10).max(1)
    ));
    out
}

/// What building did.
#[derive(Debug, Default)]
pub struct Report {
    pub roles: usize,
    pub channels: usize,
    pub failed: Vec<String>,
}

/// Builds `s` on `guild`: server look, @everyone's permissions, roles (highest first, so each new
/// one lands below the previous: the order comes out right), then channels in file order with
/// their overrides mapped to the new role IDs. Failures are collected, not fatal.
pub async fn build(
    s: &Structure,
    chat: &dyn ChatAdapter,
    http: &reqwest::Client,
    guild: &str,
) -> Report {
    let mut report = Report::default();

    let icon = match &s.icon_url {
        Some(url) => download(http, url).await,
        None => None,
    };
    let banner = match &s.banner_url {
        Some(url) => download(http, url).await,
        None => None,
    };
    if let Err(e) = chat.set_guild_look(guild, &s.name, icon, banner).await {
        report.failed.push(format!("server name/icon/banner: {e}"));
    }
    if let Err(e) = chat
        .set_everyone_permissions(guild, &fluxer_bits(&s.everyone_permissions))
        .await
    {
        report.failed.push(format!("@everyone permissions: {e}"));
    }

    let mut role_ids: HashMap<String, String> = HashMap::new();
    role_ids.insert(s.everyone_id.clone(), guild.to_owned());
    for r in &s.roles {
        let spec = RoleSpec {
            permissions: fluxer_bits(&r.permissions),
            ..r.clone()
        };
        match chat.create_role(guild, &spec).await {
            Ok(id) => {
                role_ids.insert(r.id.clone(), id);
                report.roles += 1;
            }
            Err(e) => report.failed.push(format!("role {}: {e}", r.name)),
        }
    }

    let mut channel_ids: HashMap<String, String> = HashMap::new();
    for c in &s.channels {
        let parent = c.parent.as_ref().and_then(|p| channel_ids.get(p)).cloned();
        let overwrites: Vec<Overwrite> = c
            .overwrites
            .iter()
            .filter_map(|o| {
                role_ids.get(&o.role).map(|id| Overwrite {
                    role: id.clone(),
                    allow: fluxer_bits(&o.allow),
                    deny: fluxer_bits(&o.deny),
                })
            })
            .collect();
        match chat
            .create_channel(guild, c, parent.as_deref(), &overwrites)
            .await
        {
            Ok(id) => {
                channel_ids.insert(c.id.clone(), id);
                report.channels += 1;
            }
            Err(e) => report.failed.push(format!("#{}: {e}", c.name)),
        }
    }
    report
}

async fn download(http: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    let resp = http.get(url).send().await.ok()?.error_for_status().ok()?;
    resp.bytes().await.ok().map(|b| b.to_vec())
}

// --- the command ----------------------------------------------------------------------------------

type PendingKey = (Platform, String, String);

/// `admin structure export | import | confirm | cancel`. Results go by DM.
pub struct StructureCommand {
    http: reqwest::Client,
    pending: Mutex<HashMap<PendingKey, (Instant, Structure)>>,
}

impl StructureCommand {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            pending: Mutex::default(),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<PendingKey, (Instant, Structure)>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn usage(p: &str) -> String {
        format!(
            "Copy a server's channels, roles and channel permissions to another server:\n\
             `{p}structure export`: I DM you this server's structure as a file (admins)\n\
             `{p}structure import` with that file attached, on the new server: I DM you a \
             preview (server owner)\n\
             `{p}structure confirm` builds it; `{p}structure cancel` drops it.\n\
             Members, their roles and messages aren't copied."
        )
    }

    async fn export(&self, ctx: &CommandContext, guild: &str) -> Result<()> {
        if ctx.level < Level::Admin {
            return Err(Error::Usage(format!(
                "You need to be {} to do that.",
                Level::Admin
            )));
        }
        let s = ctx.chat.read_structure(guild).await?;
        let json = serde_json::to_vec_pretty(&s)
            .map_err(|e| Error::internal("writing the structure", e))?;
        let text = format!(
            "Structure of **{}**: {} roles, {} channels. Import it on the new server with \
             `{}structure import` and this file attached.",
            s.name,
            s.roles.len(),
            s.channels.len(),
            ctx.prefix
        );
        ctx.reply_dm(&text, Some((&file_name(&s), json))).await
    }

    async fn import(&self, ctx: &CommandContext, guild: &str) -> Result<()> {
        if ctx.level < Level::ServerOwner {
            return Err(Error::Usage(format!(
                "You need to be {} to do that.",
                Level::ServerOwner
            )));
        }
        let Some(file) = ctx.message.attachments.first() else {
            return Err(Error::Usage(Self::usage(&ctx.prefix)));
        };
        let bytes = download(&self.http, &file.url)
            .await
            .ok_or_else(|| Error::Usage("I couldn't download the file.".into()))?;
        let s = parse(&bytes).map_err(Error::Usage)?;
        let roles = ctx.chat.roles(guild).await?;
        let channels = ctx.chat.text_channels(guild).await.unwrap_or_default();
        let existing_roles = roles.iter().filter(|r| r.id != guild).count();
        let text = preview(&s, &ctx.prefix, existing_roles, channels.len());
        self.pending().insert(
            (
                ctx.message.platform,
                guild.to_owned(),
                ctx.message.author.id.clone(),
            ),
            (Instant::now(), s),
        );
        ctx.reply_dm(&text, None).await
    }

    async fn confirm(&self, ctx: &CommandContext, guild: &str) -> Result<()> {
        let key = (
            ctx.message.platform,
            guild.to_owned(),
            ctx.message.author.id.clone(),
        );
        let Some((_, s)) = self
            .pending()
            .remove(&key)
            .filter(|(at, _)| at.elapsed() < PENDING_FOR)
        else {
            return Err(Error::Usage(format!(
                "Nothing to confirm: run `{}structure import` with the file first.",
                ctx.prefix
            )));
        };
        ctx.reply("Building the structure now; I'll DM you when it's done.")
            .await?;
        let report = build(&s, &*ctx.chat, &self.http, guild).await;
        let mut text = format!(
            "Structure from **{}** built: {} role(s), {} channel(s).",
            s.name, report.roles, report.channels
        );
        if !report.failed.is_empty() {
            text.push_str(&format!("\nNot done:\n• {}", report.failed.join("\n• ")));
        }
        text.push_str(&format!(
            "\nbotsu's own alerts, roles and settings: `{}export` on the old server, then \
             `{}import` here (the names now match).",
            ctx.prefix, ctx.prefix
        ));
        if text.chars().count() > 1900 {
            text = text.chars().take(1880).collect::<String>() + "\n…";
        }
        if let Err(e) = ctx.chat.send_dm(&ctx.message.author.id, &text).await {
            tracing::warn!(error = %e, "structure report DM failed");
            ctx.reply(&text).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Command for StructureCommand {
    fn name(&self) -> &'static str {
        "structure"
    }

    fn help(&self) -> &'static str {
        "Copy this server's channels, roles and channel permissions to another server, by DM (admins; import: server owner)."
    }

    fn level(&self) -> Level {
        Level::Admin
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
            Some("export") => self.export(&ctx, &guild).await,
            Some("import") => self.import(&ctx, &guild).await,
            Some("confirm") => self.confirm(&ctx, &guild).await,
            Some("cancel") => {
                let had = self.pending().remove(&key).is_some();
                ctx.reply(if had {
                    "Dropped; nothing was changed."
                } else {
                    "There was no structure import waiting."
                })
                .await?;
                Ok(())
            }
            _ => {
                ctx.reply(&Self::usage(&ctx.prefix)).await?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Structure {
        Structure {
            botsu_structure: FORMAT_VERSION,
            exported_at: "2026-11-27T10:00:00Z".into(),
            from_platform: "Discord".into(),
            name: "Old Server".into(),
            icon_url: None,
            banner_url: None,
            everyone_permissions: ((1u64 << 10) | (1u64 << 11) | (1u64 << 19)).to_string(),
            everyone_id: "1".into(),
            roles: vec![
                RoleSpec {
                    id: "10".into(),
                    name: "Admins".into(),
                    color: 0xff0000,
                    permissions: (1u64 << 3).to_string(),
                    hoist: true,
                    mentionable: false,
                },
                RoleSpec {
                    id: "11".into(),
                    name: "Members".into(),
                    color: 0,
                    permissions: "0".into(),
                    hoist: false,
                    mentionable: true,
                },
            ],
            channels: vec![
                ChannelSpec {
                    id: "20".into(),
                    kind: ChannelKind::Category,
                    name: "Staff".into(),
                    topic: None,
                    parent: None,
                    nsfw: false,
                    slowmode: 0,
                    bitrate: None,
                    user_limit: None,
                    overwrites: vec![
                        Overwrite {
                            role: "1".into(),
                            allow: "0".into(),
                            deny: (1u64 << 10).to_string(),
                        },
                        Overwrite {
                            role: "10".into(),
                            allow: (1u64 << 10).to_string(),
                            deny: "0".into(),
                        },
                    ],
                    converted_from: None,
                },
                ChannelSpec {
                    id: "21".into(),
                    kind: ChannelKind::Text,
                    name: "mod-chat".into(),
                    topic: Some("Staff only".into()),
                    parent: Some("20".into()),
                    nsfw: false,
                    slowmode: 5,
                    bitrate: None,
                    user_limit: None,
                    overwrites: vec![Overwrite {
                        role: "99".into(), // a member override or deleted role: dropped
                        allow: "0".into(),
                        deny: "0".into(),
                    }],
                    converted_from: Some("forum channel, made a text channel".into()),
                },
            ],
        }
    }

    #[test]
    fn file_round_trip_and_version_check() {
        let s = sample();
        let json = serde_json::to_vec(&s).unwrap();
        assert_eq!(parse(&json).unwrap(), s);
        let mut newer = s.clone();
        newer.botsu_structure = 2;
        assert!(parse(&serde_json::to_vec(&newer).unwrap())
            .unwrap_err()
            .contains("format 2"));
        assert!(parse(b"{}").is_err());
    }

    #[test]
    fn undefined_permission_bits_are_dropped() {
        assert_eq!(
            fluxer_bits(&((1u64 << 10) | (1u64 << 19)).to_string()),
            (1u64 << 10).to_string()
        );
        assert_eq!(fluxer_bits("garbage"), "0");
        const { assert!(FLUXER_PERMISSIONS & (1 << 40) != 0 && FLUXER_PERMISSIONS & (1 << 19) == 0) };
    }

    #[test]
    fn preview_counts_and_notes() {
        let p = preview(&sample(), "$admin ", 1, 2);
        assert!(
            p.contains("2 roles") && p.contains("1 categories, 1 text"),
            "{p}"
        );
        assert!(
            p.contains("#mod-chat (forum channel, made a text channel)"),
            "{p}"
        );
        assert!(p.contains("$admin structure confirm"));
    }

    /// Records what it's asked to create; IDs are "new-<old name>".
    #[derive(Default)]
    struct Builder {
        log: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ChatAdapter for Builder {
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
        async fn set_guild_look(
            &self,
            _: &str,
            name: &str,
            _: Option<Vec<u8>>,
            _: Option<Vec<u8>>,
        ) -> Result<()> {
            self.log.lock().unwrap().push(format!("look {name}"));
            Ok(())
        }
        async fn set_everyone_permissions(&self, _: &str, p: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("everyone {p}"));
            Ok(())
        }
        async fn create_role(&self, _: &str, r: &RoleSpec) -> Result<String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("role {} {}", r.name, r.permissions));
            Ok(format!("new-{}", r.name))
        }
        async fn create_channel(
            &self,
            _: &str,
            c: &ChannelSpec,
            parent: Option<&str>,
            overwrites: &[Overwrite],
        ) -> Result<String> {
            let o: Vec<String> = overwrites
                .iter()
                .map(|o| format!("{}:{}/{}", o.role, o.allow, o.deny))
                .collect();
            self.log.lock().unwrap().push(format!(
                "channel {} parent={} [{}]",
                c.name,
                parent.unwrap_or("-"),
                o.join(" ")
            ));
            Ok(format!("new-{}", c.name))
        }
    }

    #[tokio::test]
    async fn build_maps_roles_channels_and_overrides() {
        let b = Builder::default();
        let report = build(&sample(), &b, &reqwest::Client::new(), "G").await;
        assert_eq!((report.roles, report.channels), (2, 2));
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        let log = b.log.lock().unwrap().clone();
        let view = 1u64 << 10;
        assert_eq!(
            log,
            vec![
                "look Old Server".to_owned(),
                // bit 19 isn't a Fluxer permission: dropped.
                format!("everyone {}", view | (1 << 11)),
                // Highest first.
                format!("role Admins {}", 1u64 << 3),
                "role Members 0".to_owned(),
                // @everyone maps to the new server's ID, Admins to its new role.
                format!("channel Staff parent=- [G:0/{view} new-Admins:{view}/0]"),
                // Parent mapped; the override for an unknown role is left out.
                "channel mod-chat parent=new-Staff []".to_owned(),
            ]
        );
    }

    #[test]
    fn file_names() {
        assert_eq!(
            file_name(&sample()),
            "botsu-structure-old-server-2026-11-27.json"
        );
    }
}
