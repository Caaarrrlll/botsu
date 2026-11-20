//! `tube` and `twitch` commands: add, remove and list subscriptions for the current server.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::import::{self, Target};
use super::{live_text, mention_text, parse_channel, twitch, youtube, BASELINE, PLACEHOLDERS};
use crate::command::{Command, CommandContext};
use crate::error::{Error, Result};
use crate::permissions::Level;
use crate::storage::{AlertKind, NewSubscription, Scope, Store, Subscription};

/// Splits `"<sub> <source> [#channel]"` into the subcommand, the source, and the channel if one
/// was given as the last word.
fn split_args(ctx: &CommandContext) -> (String, String, Option<String>) {
    let (sub, rest) = split_sub(ctx);
    let (source, channel) = split_channel(rest);
    (sub, source, channel)
}

/// The subcommand (lower case) and everything after it, line breaks kept.
fn split_sub(ctx: &CommandContext) -> (String, &str) {
    let args = ctx.args.trim_start();
    match args.split_once(char::is_whitespace) {
        Some((sub, rest)) => (sub.to_lowercase(), rest.trim()),
        None => (args.to_lowercase(), ""),
    }
}

/// `"<source> [#channel]"` → the source and the channel, if the last word is one.
fn split_channel(text: &str) -> (String, Option<String>) {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let channel = words.last().and_then(|w| parse_channel(w));
    if channel.is_some() {
        words.pop();
    }
    (words.join(" "), channel)
}

/// Keeps the subscriptions posting to `channel`, if one was given.
fn in_channel(found: Vec<Subscription>, channel: Option<&str>) -> Vec<Subscription> {
    match channel {
        Some(c) => found.into_iter().filter(|s| s.channel_id == c).collect(),
        None => found,
    }
}

/// What `message` was asked to do.
#[derive(Debug, PartialEq, Eq)]
enum MessageArg {
    Show,
    Clear,
    Set(String),
}

/// `"<source> [#channel] = <text>"`, `"<source> [#channel] off"` or `"<source> [#channel]"`.
fn parse_message(rest: &str) -> (String, Option<String>, MessageArg) {
    if let Some((left, text)) = rest.split_once('=') {
        let (source, channel) = split_channel(left);
        let text = text.trim();
        let arg = if text.is_empty() {
            MessageArg::Show
        } else {
            MessageArg::Set(text.to_owned())
        };
        return (source, channel, arg);
    }
    let (source, channel) = split_channel(rest);
    match source.rsplit_once(' ') {
        Some((s, last)) if is_off(last) => (s.trim().to_owned(), channel, MessageArg::Clear),
        _ => (source, channel, MessageArg::Show),
    }
}

fn is_off(word: &str) -> bool {
    matches!(
        word.to_lowercase().as_str(),
        "off" | "none" | "reset" | "clear" | "default"
    )
}

/// What `mention` was asked to do.
#[derive(Debug, PartialEq, Eq)]
enum MentionArg {
    Clear,
    Everyone,
    Here,
    Role(String),
    User(String),
}

/// `"<source> [#channel] <@role | role ID | @user | everyone | here | off>"`.
/// A bare ID counts as a role.
fn parse_mention(rest: &str) -> Option<(String, Option<String>, MentionArg)> {
    let (left, last) = rest.trim().rsplit_once(char::is_whitespace)?;
    let arg = match last.to_lowercase().trim_start_matches('@') {
        "everyone" => MentionArg::Everyone,
        "here" => MentionArg::Here,
        w if is_off(w) => MentionArg::Clear,
        _ => {
            let digits = |id: &str| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
            let inner = last.strip_prefix("<@").and_then(|r| r.strip_suffix('>'));
            match inner {
                Some(r) if r.starts_with('&') && digits(&r[1..]) => {
                    MentionArg::Role(r[1..].to_owned())
                }
                Some(u) if digits(u.trim_start_matches('!')) => {
                    MentionArg::User(u.trim_start_matches('!').to_owned())
                }
                None if digits(last) => MentionArg::Role(last.to_owned()),
                _ => return None,
            }
        }
    };
    let (source, channel) = split_channel(left);
    Some((source, channel, arg))
}

/// `message`, shared by `tube` and `twitch`: show, set or clear the custom alert text of the
/// matching subscriptions (mods).
async fn set_message(
    ctx: &CommandContext,
    store: &Store,
    (what, cmd): (&str, &str),
    found: Vec<Subscription>,
    arg: MessageArg,
) -> Result<()> {
    let reply = match arg {
        MessageArg::Show => {
            let lines: Vec<String> = found
                .iter()
                .map(|s| match &s.message {
                    Some(m) => format!("• <#{}>: `{m}`", s.channel_id),
                    None => format!("• <#{}>: the default text", s.channel_id),
                })
                .collect();
            format!(
                "{what} alert text for **{}**:\n{}\nSet it with `{p}{cmd} message <name> [#channel] = <text>` \
                 ({PLACEHOLDERS}); `{p}{cmd} message <name> off` goes back to the default.",
                found[0].source_name,
                lines.join("\n"),
                p = ctx.prefix,
            )
        }
        MessageArg::Clear => {
            require(ctx, Level::Mod)?;
            let ids: Vec<i64> = found.iter().map(|s| s.id).collect();
            let n = store.set_alert_message(&ids, None)?;
            format!("Back to the default text ({n} alert(s) updated).")
        }
        MessageArg::Set(text) => {
            require(ctx, Level::Mod)?;
            if text.chars().count() > 1500 {
                return Err(Error::Usage("Keep the text under 1500 characters.".into()));
            }
            let ids: Vec<i64> = found.iter().map(|s| s.id).collect();
            let n = store.set_alert_message(&ids, Some(&text))?;
            format!("Alert text set ({n} alert(s) updated).")
        }
    };
    ctx.reply(&reply).await?;
    Ok(())
}

/// `mention`, shared by `tube` and `twitch`: who gets pinged by the matching subscriptions'
/// alerts. Roles: mods; @everyone and @here: admins.
async fn set_mention(
    ctx: &CommandContext,
    store: &Store,
    found: Vec<Subscription>,
    arg: MentionArg,
) -> Result<()> {
    let value = match arg {
        MentionArg::Clear => None,
        MentionArg::Everyone => Some("everyone".to_owned()),
        MentionArg::Here => Some("here".to_owned()),
        MentionArg::Role(id) => Some(id),
        MentionArg::User(id) => Some(format!("user:{id}")),
    };
    let level = match value.as_deref() {
        Some("everyone" | "here") => Level::Admin,
        _ => Level::Mod,
    };
    require(ctx, level)?;
    if let (Some(id), Some(guild)) = (&value, ctx.message.guild_id.as_deref()) {
        if !matches!(id.as_str(), "everyone" | "here") && !id.starts_with("user:") {
            // Unknown roles are refused; if the roles can't be read, take the ID as given.
            if let Ok(roles) = ctx.chat.roles(guild).await {
                if !roles.iter().any(|r| &r.id == id) {
                    return Err(Error::Usage(format!("`{id}` isn't a role in this server.")));
                }
            }
        }
    }
    let ids: Vec<i64> = found.iter().map(|s| s.id).collect();
    let n = store.set_alert_mention(&ids, value.as_deref())?;
    let reply = match &value {
        Some(v) if v.starts_with("user:") => format!(
            "Alerts will mention {} ({n} alert(s) updated).",
            quiet_mention(v, &mention_names(ctx, std::slice::from_ref(v)).await)
        ),
        Some(v) => format!(
            "Alerts will mention {} ({n} alert(s) updated). For it to ping, the role must be \
             mentionable or I need permission to mention everyone.",
            quiet_mention(v, &mention_names(ctx, std::slice::from_ref(v)).await)
        ),
        None => format!("Alerts won't mention anyone ({n} alert(s) updated)."),
    };
    ctx.reply(&reply).await?;
    Ok(())
}

fn require(ctx: &CommandContext, level: Level) -> Result<()> {
    if ctx.level >= level {
        Ok(())
    } else {
        Err(Error::Usage(format!("You need to be {level} to do that.")))
    }
}

/// A stored mention for replies and lists: in a code span, so it doesn't ping anyone.
/// Role and user names come from `names` (see [`mention_names`]); the ID if it isn't there.
fn quiet_mention(m: &str, names: &HashMap<String, String>) -> String {
    match m {
        "everyone" | "here" => format!("`{}`", mention_text(m)),
        m => match (names.get(m), m.strip_prefix("user:")) {
            (Some(name), Some(_)) => format!("**{name}**"),
            (Some(name), None) => format!("role **{name}**"),
            (None, Some(user)) => format!("user `{user}`"),
            (None, None) => format!("role `{m}`"),
        },
    }
}

/// Display names for stored mentions (role IDs and `user:<ID>`), looked up once per reply.
/// Lookups that fail are left out, so the ID is shown instead.
async fn mention_names(ctx: &CommandContext, mentions: &[String]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let mut roles = None;
    for m in mentions {
        let m = m.as_str();
        if names.contains_key(m) || matches!(m, "everyone" | "here") {
            continue;
        }
        if let Some(user) = m.strip_prefix("user:") {
            if let Ok(name) = ctx.chat.user_name(user).await {
                names.insert(m.to_owned(), name);
            }
            continue;
        }
        if roles.is_none() {
            roles = Some(match ctx.message.guild_id.as_deref() {
                Some(g) => ctx.chat.roles(g).await.unwrap_or_default(),
                None => Vec::new(),
            });
        }
        if let Some(r) = roles.iter().flatten().find(|r| r.id == m) {
            names.insert(m.to_owned(), r.name.clone());
        }
    }
    names
}

/// The reply when `message`/`mention`/`remove` match nothing: a mention typed where the name
/// goes gets an explanation; otherwise the names that do exist here are listed.
fn not_found(
    ctx: &CommandContext,
    store: &Store,
    (kind, what, cmd): (AlertKind, &str, &str),
    source: &str,
    channel: Option<&str>,
) -> Error {
    let p = &ctx.prefix;
    // A plain @word can be a YouTube handle, so only the <@…> / <#…> mention forms count.
    if source.starts_with("<@") || source.starts_with("<#") {
        return Error::Usage(format!(
            "`<name>` is the {what} channel as `{p}{cmd} list` shows it, not a mention. For \
             example: `{p}{cmd} mention <name> @role` or `{p}{cmd} message <name> = <text>`."
        ));
    }
    let place = channel.map_or("here".to_owned(), |c| format!("in <#{c}>"));
    let mut names: Vec<String> = store
        .list_subscriptions(kind, ctx.message.platform, scope(ctx))
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.source_name)
        .collect();
    names.dedup();
    let known = match names.len() {
        0 => format!("There are no {what} alerts here yet."),
        n if n <= 15 => format!("Alerts here: {}.", names.join(", ")),
        _ => format!("See `{p}{cmd} list`."),
    };
    Error::Usage(format!("No {what} alerts for `{source}` {place}. {known}"))
}

/// What's customised on a subscription, for `list`.
fn list_extras(s: &Subscription, names: &HashMap<String, String>) -> String {
    let mut out = String::new();
    if let Some(m) = &s.mention {
        out.push_str(&format!(" · mentions {}", quiet_mention(m, names)));
    }
    if s.message.is_some() {
        out.push_str(" · custom text");
    }
    out
}

/// For `import`: everything after the subcommand, keeping line breaks, with an optional
/// `#channel` right after `import` (`!tube import #alerts` + the pasted list on the next lines).
/// Returns the pasted body and the forced target channel, if one was given.
fn split_import(ctx: &CommandContext) -> (String, Option<String>) {
    let rest = ctx
        .args
        .split_once(char::is_whitespace)
        .map(|(_, r)| r.trim_start())
        .unwrap_or("");
    let first = rest.split_whitespace().next().unwrap_or("");
    match parse_channel(first) {
        Some(id) => (rest[first.len()..].to_owned(), Some(id)),
        None => (rest.to_owned(), None),
    }
}

/// Works out the channel a group of imported subscriptions posts to, and posts `notice` there
/// the first time it's used. `Err` holds why the group can't be imported.
async fn import_target(
    ctx: &CommandContext,
    target: &Target,
    forced: Option<&str>,
    notice: &str,
    announced: &mut Vec<String>,
) -> std::result::Result<String, String> {
    let id = match (forced, target) {
        (Some(f), _) => f.to_owned(),
        (None, Target::Default) => ctx.message.channel_id.clone(),
        (None, Target::Id(id)) => id.clone(),
        (None, Target::Name(name)) => {
            let Some(guild) = ctx.message.guild_id.as_deref() else {
                return Err(format!("#{name}: channel names only work inside a server"));
            };
            match ctx.chat.find_channel(guild, name).await {
                Ok(Some(id)) => id,
                Ok(None) => return Err(format!("#{name}: no channel with that name here")),
                Err(e) => return Err(format!("#{name}: couldn't look up channels ({e})")),
            }
        }
    };
    if !announced.contains(&id) {
        // Pasted lists carry channel IDs; never act on a channel in another server.
        if ctx.ensure_channel_here(&id).await.is_err() {
            return Err(format!("<#{id}>: not a channel in this server"));
        }
        if let Err(e) = ctx.chat.send_message(&id, notice).await {
            tracing::info!(error = %e, channel = %id, "test post to import channel failed");
            return Err(format!("<#{id}>: I can't post there"));
        }
        announced.push(id.clone());
    }
    Ok(id)
}

/// Builds the result message of an import, kept under Discord's 2000-character limit.
fn import_summary(kind: &str, added: &[String], existing: &[String], failed: &[String]) -> String {
    let mut parts = vec![format!(
        "{kind} import: {} added, {} already there, {} skipped.",
        added.len(),
        existing.len(),
        failed.len()
    )];
    if !added.is_empty() {
        parts.push(format!("Added:\n• {}", added.join("\n• ")));
    }
    if !failed.is_empty() {
        parts.push(format!("Skipped:\n• {}", failed.join("\n• ")));
    }
    let text = parts.join("\n");
    if text.chars().count() <= 1900 {
        text
    } else {
        let cut: String = text.chars().take(1880).collect();
        format!("{cut}\n… (truncated)")
    }
}

// Permissions (kept lax for now, phase 4 adds roles): add, list and remove are open to anyone;
// import and `tube shorts` need a bot owner (ctx.require_owner(), BOTSU_OWNERS).

fn scope(ctx: &CommandContext) -> Scope<'_> {
    Scope::of(ctx.message.guild_id.as_deref(), &ctx.message.channel_id)
}

/// Posts a notice in the target channel before saving the subscription, which also proves
/// the bot is allowed to post there.
async fn announce_in_target(ctx: &CommandContext, target: &str, text: &str) -> Result<()> {
    ctx.ensure_channel_here(target).await?;
    ctx.chat
        .send_message(target, text)
        .await
        .map(|_| ())
        .map_err(|e| {
            tracing::info!(error = %e, channel = target, "test post to alert channel failed");
            Error::Usage(format!(
            "I can't post in <#{target}>. Give me permission to send messages there and try again."
        ))
        })
}

// ---------------------------------------------------------------------------------------------

pub struct TubeCommand {
    store: Arc<Store>,
    http: reqwest::Client,
    /// None without YOUTUBE_API_KEY: only channel IDs/links can be added.
    api: Option<Arc<youtube::YouTubeApi>>,
}

impl TubeCommand {
    pub fn new(
        store: Arc<Store>,
        http: reqwest::Client,
        api: Option<Arc<youtube::YouTubeApi>>,
    ) -> Self {
        Self { store, http, api }
    }

    fn usage(p: &str) -> String {
        format!(
            "YouTube upload alerts:\n\
             `{p}tube add <channel link | @handle | channel ID> [#channel]`\n\
             `{p}tube remove <name | @handle | channel ID> [#channel]`\n\
             `{p}tube list`\n\
             `{p}tube shorts <name | channel ID> on|off` (Shorts are off by default)\n\
             `{p}tube message <name> [#channel] = <text>` / `off`: custom alert text (mods)\n\
             `{p}tube mention <name> [#channel] <@role | @user | everyone | here | off>` (mods; everyone/here: admins)\n\
             `{p}tube import [#channel]` + a pasted list with channel IDs on the next lines"
        )
    }

    async fn add(&self, ctx: &CommandContext, source: &str, target: &str) -> Result<()> {
        if source.is_empty() {
            return Err(Error::Usage(Self::usage(&ctx.prefix)));
        }
        let (channel_id, key) = youtube::resolve_channel(self.api.as_deref(), source)
            .await
            .map_err(Error::Usage)?;
        let feed = youtube::fetch_feed(&self.http, &channel_id)
            .await
            .map_err(|e| Error::Usage(format!("Found channel `{channel_id}` but {e}.")))?;
        let name = if feed.channel_name.is_empty() {
            channel_id.clone()
        } else {
            feed.channel_name.clone()
        };

        announce_in_target(
            ctx,
            target,
            &format!("New uploads from **{name}** will be posted here."),
        )
        .await?;

        // Everything currently in the feed counts as already posted: only new uploads alert.
        self.store
            .mark_seen(AlertKind::YouTube, &channel_id, BASELINE)?;
        for v in &feed.videos {
            self.store
                .mark_seen(AlertKind::YouTube, &channel_id, &v.id)?;
        }
        let added = self.store.add_subscription(&NewSubscription {
            kind: AlertKind::YouTube,
            source_id: &channel_id,
            source_name: &name,
            source_key: &key,
            platform: ctx.message.platform,
            guild_id: ctx.message.guild_id.as_deref(),
            channel_id: target,
            created_by: &ctx.message.author.id,
        })?;

        let mut reply = if added {
            format!("Subscribed <#{target}> to **{name}**.")
        } else {
            format!("<#{target}> was already subscribed to **{name}**.")
        };
        if let Some(latest) = feed.videos.first() {
            reply.push_str(&format!(
                " Latest video: **{}** (won't be reposted).",
                latest.title
            ));
        }
        ctx.reply(&reply).await?;
        Ok(())
    }

    async fn import(&self, ctx: &CommandContext, body: &str, forced: Option<&str>) -> Result<()> {
        ctx.require_owner()?;
        let groups = import::parse_youtube(body);
        if groups.is_empty() {
            return Err(Error::Usage(format!(
                "No YouTube channel IDs (`UC...`) found. Paste the list on the lines after \
                 `{}tube import`, for example a line like `Name (UC8AUKEbnzq_SoCjRhsm5k_A)`.",
                ctx.prefix
            )));
        }

        let (mut added, mut existing, mut failed, mut announced) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for group in &groups {
            let channel = match import_target(
                ctx,
                &group.target,
                forced,
                "New uploads from YouTube channels will be posted here.",
                &mut announced,
            )
            .await
            {
                Ok(c) => c,
                Err(why) => {
                    let names: Vec<String> = group
                        .items
                        .iter()
                        .map(|e| e.name.clone().unwrap_or_else(|| e.channel_id.clone()))
                        .collect();
                    failed.push(format!("{} ({why})", names.join(", ")));
                    continue;
                }
            };
            for entry in &group.items {
                let label = entry
                    .name
                    .clone()
                    .unwrap_or_else(|| entry.channel_id.clone());
                // Fetching the feed checks the channel exists and lets us skip its current videos.
                let feed = match youtube::fetch_feed(&self.http, &entry.channel_id).await {
                    Ok(f) => f,
                    Err(e) => {
                        failed.push(format!("{label} (`{}`): {e}", entry.channel_id));
                        continue;
                    }
                };
                let name = if feed.channel_name.is_empty() {
                    label
                } else {
                    feed.channel_name.clone()
                };
                self.store
                    .mark_seen(AlertKind::YouTube, &entry.channel_id, BASELINE)?;
                for v in &feed.videos {
                    self.store
                        .mark_seen(AlertKind::YouTube, &entry.channel_id, &v.id)?;
                }
                let new = self.store.add_subscription(&NewSubscription {
                    kind: AlertKind::YouTube,
                    source_id: &entry.channel_id,
                    source_name: &name,
                    source_key: &entry.channel_id,
                    platform: ctx.message.platform,
                    guild_id: ctx.message.guild_id.as_deref(),
                    channel_id: &channel,
                    created_by: &ctx.message.author.id,
                })?;
                let line = format!("{name} → <#{channel}>");
                if new { &mut added } else { &mut existing }.push(line);
            }
        }
        ctx.reply(&import_summary("YouTube", &added, &existing, &failed))
            .await?;
        Ok(())
    }

    /// This server's subscriptions matching a name, handle, link or ID.
    async fn find(&self, ctx: &CommandContext, source: &str) -> Result<Vec<Subscription>> {
        let platform = ctx.message.platform;
        let found =
            self.store
                .find_subscriptions(AlertKind::YouTube, platform, scope(ctx), source)?;
        if !found.is_empty() {
            return Ok(found);
        }
        // A link or handle that wasn't stored as typed: resolve it to the channel ID.
        match youtube::resolve_channel(self.api.as_deref(), source).await {
            Ok((id, _)) => {
                Ok(self
                    .store
                    .find_subscriptions(AlertKind::YouTube, platform, scope(ctx), &id)?)
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// `shorts <name | channel ID> on|off`: whether that channel's Shorts are posted too.
    /// Bot owners only.
    async fn shorts(&self, ctx: &CommandContext, args: &str) -> Result<()> {
        ctx.require_owner()?;
        let (source, setting) = args
            .trim()
            .rsplit_once(char::is_whitespace)
            .ok_or_else(|| Error::Usage(Self::usage(&ctx.prefix)))?;
        let on = match setting.to_lowercase().as_str() {
            "on" | "yes" | "true" | "enable" => true,
            "off" | "no" | "false" | "disable" => false,
            _ => return Err(Error::Usage(Self::usage(&ctx.prefix))),
        };
        let source = source.trim();
        let found = self.find(ctx, source).await?;
        if found.is_empty() {
            return Err(Error::Usage(format!(
                "No YouTube alerts for `{source}` here. See `{}tube list`.",
                ctx.prefix
            )));
        }
        let ids: Vec<i64> = found.iter().map(|s| s.id).collect();
        let n = self.store.set_include_shorts(&ids, on)?;
        let what = if on {
            "Shorts will be posted too"
        } else {
            "Shorts won't be posted"
        };
        ctx.reply(&format!("{what} ({n} alert(s) updated)."))
            .await?;
        Ok(())
    }

    /// Like `find`, but an empty result is an error for the user. `channel` narrows it down.
    async fn find_some(
        &self,
        ctx: &CommandContext,
        source: &str,
        channel: Option<&str>,
    ) -> Result<Vec<Subscription>> {
        if source.is_empty() {
            return Err(Error::Usage(Self::usage(&ctx.prefix)));
        }
        let found = in_channel(self.find(ctx, source).await?, channel);
        if found.is_empty() {
            let what = (AlertKind::YouTube, "YouTube", "tube");
            return Err(not_found(ctx, &self.store, what, source, channel));
        }
        Ok(found)
    }

    async fn remove(
        &self,
        ctx: &CommandContext,
        source: &str,
        channel: Option<&str>,
    ) -> Result<()> {
        let found = self.find_some(ctx, source, channel).await?;
        let ids: Vec<i64> = found.iter().map(|s| s.id).collect();
        let n = self.store.delete_subscriptions(&ids)?;
        ctx.reply(&format!("Removed {n} YouTube alert(s).")).await?;
        Ok(())
    }
}

#[async_trait]
impl Command for TubeCommand {
    fn name(&self) -> &'static str {
        "tube"
    }

    fn help(&self) -> &'static str {
        "YouTube upload alerts: add, remove, list, message, mention."
    }

    // `message` and `mention` need mod; the rest is open to everyone.
    fn needs_level(&self) -> bool {
        true
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        if ctx
            .args
            .split_whitespace()
            .next()
            .map(str::to_lowercase)
            .as_deref()
            == Some("import")
        {
            let (body, target) = split_import(&ctx);
            return self.import(&ctx, &body, target.as_deref()).await;
        }
        let (sub, source, target) = split_args(&ctx);
        match sub.as_str() {
            "add" | "subscribe" => {
                let target = target.unwrap_or_else(|| ctx.message.channel_id.clone());
                self.add(&ctx, &source, &target).await
            }
            "remove" | "unsubscribe" | "delete" | "del" => {
                self.remove(&ctx, &source, target.as_deref()).await
            }
            "shorts" => self.shorts(&ctx, split_sub(&ctx).1).await,
            "message" | "text" => {
                let (source, channel, arg) = parse_message(split_sub(&ctx).1);
                let found = self.find_some(&ctx, &source, channel.as_deref()).await?;
                set_message(&ctx, &self.store, ("YouTube", "tube"), found, arg).await
            }
            "mention" | "ping" => {
                let (source, channel, arg) = parse_mention(split_sub(&ctx).1)
                    .ok_or_else(|| Error::Usage(Self::usage(&ctx.prefix)))?;
                let found = self.find_some(&ctx, &source, channel.as_deref()).await?;
                set_mention(&ctx, &self.store, found, arg).await
            }
            "list" => {
                let subs = self.store.list_subscriptions(
                    AlertKind::YouTube,
                    ctx.message.platform,
                    scope(&ctx),
                )?;
                let mentions: Vec<String> = subs.iter().filter_map(|s| s.mention.clone()).collect();
                let names = mention_names(&ctx, &mentions).await;
                let text = if subs.is_empty() {
                    format!(
                        "No YouTube alerts here yet. Add one with `{}tube add <channel>`.",
                        ctx.prefix
                    )
                } else {
                    let lines: Vec<String> = subs
                        .iter()
                        .map(|s| {
                            format!(
                                "• **{}** (`{}`) → <#{}>{}{}",
                                s.source_name,
                                s.source_id,
                                s.channel_id,
                                if s.include_shorts { " (Shorts on)" } else { "" },
                                list_extras(s, &names)
                            )
                        })
                        .collect();
                    format!("YouTube alerts:\n{}", lines.join("\n"))
                };
                ctx.reply(&text).await?;
                Ok(())
            }
            _ => Err(Error::Usage(Self::usage(&ctx.prefix))),
        }
    }
}

// ---------------------------------------------------------------------------------------------

pub struct TwitchCommand {
    store: Arc<Store>,
    /// None when TWITCH_CLIENT_ID / TWITCH_CLIENT_SECRET aren't set.
    twitch: Option<Arc<twitch::Twitch>>,
}

impl TwitchCommand {
    pub fn new(store: Arc<Store>, twitch: Option<Arc<twitch::Twitch>>) -> Self {
        Self { store, twitch }
    }

    fn usage(p: &str) -> String {
        format!(
            "Twitch live alerts:\n\
             `{p}twitch add <name | twitch.tv link> [#channel]`\n\
             `{p}twitch remove <name> [#channel]`\n\
             `{p}twitch list`\n\
             `{p}twitch message <name> [#channel] = <text>` / `off`: custom alert text (mods)\n\
             `{p}twitch mention <name> [#channel] <@role | @user | everyone | here | off>` (mods; everyone/here: admins)\n\
             `{p}twitch import [#channel]` + a pasted list of names on the next lines"
        )
    }

    /// This server's subscriptions for a streamer (name or link), narrowed to `channel` if
    /// given. Empty is an error for the user.
    fn find_some(
        &self,
        ctx: &CommandContext,
        source: &str,
        channel: Option<&str>,
    ) -> Result<Vec<Subscription>> {
        if source.is_empty() {
            return Err(Error::Usage(Self::usage(&ctx.prefix)));
        }
        let login = twitch::parse_login(source).unwrap_or_else(|| source.to_owned());
        let found = self.store.find_subscriptions(
            AlertKind::Twitch,
            ctx.message.platform,
            scope(ctx),
            &login,
        )?;
        let found = in_channel(found, channel);
        if found.is_empty() {
            let what = (AlertKind::Twitch, "Twitch", "twitch");
            return Err(not_found(ctx, &self.store, what, source, channel));
        }
        Ok(found)
    }

    fn client(&self) -> Result<&twitch::Twitch> {
        self.twitch.as_deref().ok_or_else(|| {
            Error::Usage(
                "Twitch alerts aren't set up: the bot's host needs to set TWITCH_CLIENT_ID and \
                 TWITCH_CLIENT_SECRET."
                    .into(),
            )
        })
    }

    async fn add(&self, ctx: &CommandContext, source: &str, target: &str) -> Result<()> {
        let client = self.client()?;
        let login =
            twitch::parse_login(source).ok_or_else(|| Error::Usage(Self::usage(&ctx.prefix)))?;
        let user = client
            .user_by_login(&login)
            .await
            .map_err(|e| Error::internal("Twitch user lookup", e))?
            .ok_or_else(|| Error::Usage(format!("There's no Twitch user called `{login}`.")))?;

        announce_in_target(
            ctx,
            target,
            &format!(
                "**{}** going live on Twitch will be posted here.",
                user.display_name
            ),
        )
        .await?;

        // If they're live right now, that broadcast counts as already announced.
        let live = client
            .live_streams(std::slice::from_ref(&user.id))
            .await
            .map_err(|e| Error::internal("Twitch stream lookup", e))?;
        self.store
            .mark_seen(AlertKind::Twitch, &user.id, BASELINE)?;
        for s in &live {
            self.store.mark_seen(AlertKind::Twitch, &user.id, &s.id)?;
            self.store
                .set_last_live_at(AlertKind::Twitch, &user.id, crate::storage::now())?;
        }

        let added = self.store.add_subscription(&NewSubscription {
            kind: AlertKind::Twitch,
            source_id: &user.id,
            source_name: &user.display_name,
            source_key: &user.login,
            platform: ctx.message.platform,
            guild_id: ctx.message.guild_id.as_deref(),
            channel_id: target,
            created_by: &ctx.message.author.id,
        })?;

        let mut reply = if added {
            format!("Subscribed <#{target}> to **{}**.", user.display_name)
        } else {
            format!(
                "<#{target}> was already subscribed to **{}**.",
                user.display_name
            )
        };
        match live.first() {
            Some(s) => reply.push_str(&format!(
                " They're live right now (not announced, since it already started):\n{}",
                live_text(s)
            )),
            None => reply.push_str(" They're offline right now."),
        }
        ctx.reply(&reply).await?;
        Ok(())
    }

    async fn import(&self, ctx: &CommandContext, body: &str, forced: Option<&str>) -> Result<()> {
        ctx.require_owner()?;
        let client = self.client()?;
        let (groups, skipped_words) = import::parse_twitch(body);
        if groups.is_empty() {
            return Err(Error::Usage(format!(
                "No Twitch names found. Paste them on the lines after `{}twitch import` \
                 (one per line, or `a, b and c`), optionally under `#channel` lines.",
                ctx.prefix
            )));
        }

        let logins: Vec<String> = groups.iter().flat_map(|g| g.items.clone()).collect();
        let users = client
            .users_by_login(&logins)
            .await
            .map_err(|e| Error::internal("Twitch user lookup", e))?;
        let mut failed: Vec<String> = skipped_words
            .iter()
            .map(|w| format!("`{w}`: not a Twitch name"))
            .collect();

        // Streams already running count as announced, like `twitch add`.
        let ids: Vec<String> = users.iter().map(|u| u.id.clone()).collect();
        let live = client
            .live_streams(&ids)
            .await
            .map_err(|e| Error::internal("Twitch stream lookup", e))?;
        let now = crate::storage::now();
        for s in &live {
            self.store.mark_seen(AlertKind::Twitch, &s.user_id, &s.id)?;
            self.store
                .set_last_live_at(AlertKind::Twitch, &s.user_id, now)?;
        }

        let (mut added, mut existing, mut announced) = (Vec::new(), Vec::new(), Vec::new());
        for group in &groups {
            let channel = match import_target(
                ctx,
                &group.target,
                forced,
                "Twitch streamers going live will be posted here.",
                &mut announced,
            )
            .await
            {
                Ok(c) => c,
                Err(why) => {
                    failed.push(format!("{} ({why})", group.items.join(", ")));
                    continue;
                }
            };
            for login in &group.items {
                let Some(user) = users.iter().find(|u| u.login.eq_ignore_ascii_case(login)) else {
                    failed.push(format!("`{login}`: no such Twitch user"));
                    continue;
                };
                self.store
                    .mark_seen(AlertKind::Twitch, &user.id, BASELINE)?;
                let new = self.store.add_subscription(&NewSubscription {
                    kind: AlertKind::Twitch,
                    source_id: &user.id,
                    source_name: &user.display_name,
                    source_key: &user.login,
                    platform: ctx.message.platform,
                    guild_id: ctx.message.guild_id.as_deref(),
                    channel_id: &channel,
                    created_by: &ctx.message.author.id,
                })?;
                let line = format!("{} → <#{channel}>", user.display_name);
                if new { &mut added } else { &mut existing }.push(line);
            }
        }
        ctx.reply(&import_summary("Twitch", &added, &existing, &failed))
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Command for TwitchCommand {
    fn name(&self) -> &'static str {
        "twitch"
    }

    fn help(&self) -> &'static str {
        "Twitch live alerts: add, remove, list, message, mention."
    }

    // `message` and `mention` need mod; the rest is open to everyone.
    fn needs_level(&self) -> bool {
        true
    }

    async fn run(&self, ctx: CommandContext) -> Result<()> {
        if ctx
            .args
            .split_whitespace()
            .next()
            .map(str::to_lowercase)
            .as_deref()
            == Some("import")
        {
            let (body, target) = split_import(&ctx);
            return self.import(&ctx, &body, target.as_deref()).await;
        }
        let (sub, source, target) = split_args(&ctx);
        match sub.as_str() {
            "add" | "subscribe" => {
                let target = target.unwrap_or_else(|| ctx.message.channel_id.clone());
                self.add(&ctx, &source, &target).await
            }
            "message" | "text" => {
                let (source, channel, arg) = parse_message(split_sub(&ctx).1);
                let found = self.find_some(&ctx, &source, channel.as_deref())?;
                set_message(&ctx, &self.store, ("Twitch", "twitch"), found, arg).await
            }
            "mention" | "ping" => {
                let (source, channel, arg) = parse_mention(split_sub(&ctx).1)
                    .ok_or_else(|| Error::Usage(Self::usage(&ctx.prefix)))?;
                let found = self.find_some(&ctx, &source, channel.as_deref())?;
                set_mention(&ctx, &self.store, found, arg).await
            }
            "remove" | "unsubscribe" | "delete" | "del" => {
                let found = self.find_some(&ctx, &source, target.as_deref())?;
                let ids: Vec<i64> = found.iter().map(|s| s.id).collect();
                let n = self.store.delete_subscriptions(&ids)?;
                ctx.reply(&format!("Removed {n} Twitch alert(s).")).await?;
                Ok(())
            }
            "list" => {
                let subs = self.store.list_subscriptions(
                    AlertKind::Twitch,
                    ctx.message.platform,
                    scope(&ctx),
                )?;
                let mentions: Vec<String> = subs.iter().filter_map(|s| s.mention.clone()).collect();
                let names = mention_names(&ctx, &mentions).await;
                let text = if subs.is_empty() {
                    format!(
                        "No Twitch alerts here yet. Add one with `{}twitch add <name>`.",
                        ctx.prefix
                    )
                } else {
                    let lines: Vec<String> = subs
                        .iter()
                        .map(|s| {
                            format!(
                                "• **{}** (twitch.tv/{}) → <#{}>{}",
                                s.source_name,
                                s.source_key,
                                s.channel_id,
                                list_extras(s, &names)
                            )
                        })
                        .collect();
                    format!("Twitch alerts:\n{}", lines.join("\n"))
                };
                ctx.reply(&text).await?;
                Ok(())
            }
            _ => Err(Error::Usage(Self::usage(&ctx.prefix))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_args() {
        assert_eq!(
            parse_message("The Axeman <#5> = {name} is up! {url}"),
            (
                "The Axeman".to_owned(),
                Some("5".to_owned()),
                MessageArg::Set("{name} is up! {url}".to_owned())
            )
        );
        assert_eq!(
            parse_message("The Axeman off"),
            ("The Axeman".to_owned(), None, MessageArg::Clear)
        );
        assert_eq!(
            parse_message("The Axeman"),
            ("The Axeman".to_owned(), None, MessageArg::Show)
        );
        assert_eq!(
            parse_message("Granty ="),
            ("Granty".to_owned(), None, MessageArg::Show)
        );
    }

    #[test]
    fn mention_args() {
        assert_eq!(
            parse_mention("Some Name <@&42>"),
            Some((
                "Some Name".to_owned(),
                None,
                MentionArg::Role("42".to_owned())
            ))
        );
        assert_eq!(
            parse_mention("abc <#7> @everyone"),
            Some(("abc".to_owned(), Some("7".to_owned()), MentionArg::Everyone))
        );
        assert_eq!(
            parse_mention("abc here"),
            Some(("abc".to_owned(), None, MentionArg::Here))
        );
        assert_eq!(
            parse_mention("abc off"),
            Some(("abc".to_owned(), None, MentionArg::Clear))
        );
        assert_eq!(
            parse_mention("abc <@!77>"),
            Some(("abc".to_owned(), None, MentionArg::User("77".to_owned())))
        );
        assert_eq!(
            parse_mention("abc 42"),
            Some(("abc".to_owned(), None, MentionArg::Role("42".to_owned())))
        );
        assert_eq!(parse_mention("abc Moderators"), None);
        assert_eq!(parse_mention("abc"), None);
    }

    #[test]
    fn mentions_shown_without_pinging() {
        let names = HashMap::from([
            ("1".to_owned(), "Testing Role".to_owned()),
            ("user:2".to_owned(), "Carl".to_owned()),
        ]);
        assert_eq!(quiet_mention("1", &names), "role **Testing Role**");
        assert_eq!(quiet_mention("user:2", &names), "**Carl**");
        assert_eq!(quiet_mention("3", &names), "role `3`");
        assert_eq!(quiet_mention("user:4", &names), "user `4`");
        assert_eq!(quiet_mention("everyone", &names), "`@everyone`");
    }

    #[test]
    fn channel_filter() {
        assert_eq!(
            split_channel("Some Name <#9>"),
            ("Some Name".to_owned(), Some("9".to_owned()))
        );
        assert_eq!(split_channel("Some Name"), ("Some Name".to_owned(), None));
    }
}
