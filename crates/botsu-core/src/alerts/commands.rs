//! `tube` and `twitch` commands: add, remove and list subscriptions for the current server.

use std::sync::Arc;

use async_trait::async_trait;

use super::import::{self, Target};
use super::{live_text, parse_channel, twitch, youtube, BASELINE};
use crate::command::{Command, CommandContext};
use crate::error::{Error, Result};
use crate::storage::{AlertKind, NewSubscription, Scope, Store, Subscription};

/// Splits `"add <source> [#channel]"` into the subcommand, the source, and the target channel
/// (the current channel when none is given).
fn split_args(ctx: &CommandContext) -> (String, String, String) {
    let mut words: Vec<&str> = ctx.args.split_whitespace().collect();
    let sub = if words.is_empty() {
        String::new()
    } else {
        words.remove(0).to_lowercase()
    };
    let target = match words.last().and_then(|w| parse_channel(w)) {
        Some(id) => {
            words.pop();
            id
        }
        None => ctx.message.channel_id.clone(),
    };
    (sub, words.join(" "), target)
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

// --- Permissions ------------------------------------------------------------------------------
//
// add, list: anyone. remove, shorts: whoever added that alert, the server owner, bot owners.
// import: bot owners only (checked with ctx.require_owner()).

const NOT_YOURS: &str =
    "Only the person who added that alert or the server owner can change or remove it.";

/// Splits matching subscriptions into those the caller may change, and how many belong to
/// someone else. The server owner is only looked up when it matters.
async fn permitted(ctx: &CommandContext, subs: Vec<Subscription>) -> (Vec<Subscription>, usize) {
    let author = &ctx.message.author.id;
    if ctx.is_owner || subs.iter().all(|s| &s.created_by == author) {
        return (subs, 0);
    }
    if is_server_owner(ctx).await {
        return (subs, 0);
    }
    let (mine, others): (Vec<_>, Vec<_>) = subs.into_iter().partition(|s| &s.created_by == author);
    (mine, others.len())
}

async fn is_server_owner(ctx: &CommandContext) -> bool {
    let Some(guild) = ctx.message.guild_id.as_deref() else {
        return false;
    };
    match ctx.chat.guild_owner(guild).await {
        Ok(owner) => owner.as_deref() == Some(ctx.message.author.id.as_str()),
        Err(e) => {
            tracing::warn!(error = %e, guild, "couldn't look up the server owner");
            false
        }
    }
}

fn left_alone(others: usize) -> String {
    match others {
        0 => String::new(),
        n => format!(
            " {n} added by someone else were left alone (only they or the server owner can change those)."
        ),
    }
}

fn scope(ctx: &CommandContext) -> Scope<'_> {
    Scope::of(ctx.message.guild_id.as_deref(), &ctx.message.channel_id)
}

/// Posts a notice in the target channel before saving the subscription, which also proves
/// the bot is allowed to post there.
async fn announce_in_target(ctx: &CommandContext, target: &str, text: &str) -> Result<()> {
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
             `{p}tube remove <name | @handle | channel ID>`\n\
             `{p}tube list`\n\
             `{p}tube shorts <name | channel ID> on|off` (Shorts are off by default)\n\
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
    /// Allowed for whoever added the alert, the server owner and bot owners.
    async fn shorts(&self, ctx: &CommandContext, args: &str) -> Result<()> {
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
        let (allowed, others) = permitted(ctx, found).await;
        if allowed.is_empty() {
            return Err(Error::Usage(NOT_YOURS.into()));
        }
        let ids: Vec<i64> = allowed.iter().map(|s| s.id).collect();
        let n = self.store.set_include_shorts(&ids, on)?;
        let what = if on {
            "Shorts will be posted too"
        } else {
            "Shorts won't be posted"
        };
        ctx.reply(&format!(
            "{what} ({n} alert(s) updated).{}",
            left_alone(others)
        ))
        .await?;
        Ok(())
    }

    /// Allowed for whoever added the alert, the server owner and bot owners.
    async fn remove(&self, ctx: &CommandContext, source: &str) -> Result<()> {
        if source.is_empty() {
            return Err(Error::Usage(Self::usage(&ctx.prefix)));
        }
        let found = self.find(ctx, source).await?;
        if found.is_empty() {
            return Err(Error::Usage(format!(
                "No YouTube alerts for `{source}` here. See `{}tube list`.",
                ctx.prefix
            )));
        }
        let (allowed, others) = permitted(ctx, found).await;
        if allowed.is_empty() {
            return Err(Error::Usage(NOT_YOURS.into()));
        }
        let ids: Vec<i64> = allowed.iter().map(|s| s.id).collect();
        let n = self.store.delete_subscriptions(&ids)?;
        ctx.reply(&format!(
            "Removed {n} YouTube alert(s).{}",
            left_alone(others)
        ))
        .await?;
        Ok(())
    }
}

#[async_trait]
impl Command for TubeCommand {
    fn name(&self) -> &'static str {
        "tube"
    }

    fn help(&self) -> &'static str {
        "YouTube upload alerts: add, remove, list."
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
            "add" | "subscribe" => self.add(&ctx, &source, &target).await,
            "remove" | "unsubscribe" | "delete" | "del" => self.remove(&ctx, &source).await,
            "shorts" => self.shorts(&ctx, &source).await,
            "list" => {
                let subs = self.store.list_subscriptions(
                    AlertKind::YouTube,
                    ctx.message.platform,
                    scope(&ctx),
                )?;
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
                                "• **{}** (`{}`) → <#{}>{}",
                                s.source_name,
                                s.source_id,
                                s.channel_id,
                                if s.include_shorts { " (Shorts on)" } else { "" }
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
             `{p}twitch remove <name>`\n\
             `{p}twitch list`\n\
             `{p}twitch import [#channel]` + a pasted list of names on the next lines"
        )
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
        "Twitch live alerts: add, remove, list."
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
            "add" | "subscribe" => self.add(&ctx, &source, &target).await,
            // Allowed for whoever added the alert, the server owner and bot owners.
            "remove" | "unsubscribe" | "delete" | "del" => {
                if source.is_empty() {
                    return Err(Error::Usage(Self::usage(&ctx.prefix)));
                }
                let login = twitch::parse_login(&source).unwrap_or(source);
                let found = self.store.find_subscriptions(
                    AlertKind::Twitch,
                    ctx.message.platform,
                    scope(&ctx),
                    &login,
                )?;
                if found.is_empty() {
                    return Err(Error::Usage(format!(
                        "No Twitch alerts for `{login}` here. See `{}twitch list`.",
                        ctx.prefix
                    )));
                }
                let (allowed, others) = permitted(&ctx, found).await;
                if allowed.is_empty() {
                    return Err(Error::Usage(NOT_YOURS.into()));
                }
                let ids: Vec<i64> = allowed.iter().map(|s| s.id).collect();
                let n = self.store.delete_subscriptions(&ids)?;
                ctx.reply(&format!(
                    "Removed {n} Twitch alert(s).{}",
                    left_alone(others)
                ))
                .await?;
                Ok(())
            }
            "list" => {
                let subs = self.store.list_subscriptions(
                    AlertKind::Twitch,
                    ctx.message.platform,
                    scope(&ctx),
                )?;
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
                                "• **{}** (twitch.tv/{}) → <#{}>",
                                s.source_name, s.source_key, s.channel_id
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
    use crate::adapter::{Author, Capabilities, ChatAdapter, IncomingMessage, Platform};

    /// A platform where the server owner is "owner" and sending always works.
    struct FakeChat;

    #[async_trait]
    impl ChatAdapter for FakeChat {
        fn platform(&self) -> Platform {
            Platform::Discord
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        async fn send_message(&self, _: &str, _: &str) -> Result<String> {
            Ok("m".into())
        }
        async fn guild_owner(&self, _: &str) -> Result<Option<String>> {
            Ok(Some("owner".into()))
        }
    }

    fn ctx(author: &str, is_owner: bool) -> CommandContext {
        CommandContext {
            chat: Arc::new(FakeChat),
            message: IncomingMessage {
                platform: Platform::Discord,
                guild_id: Some("g".into()),
                channel_id: "c".into(),
                message_id: "m".into(),
                author: Author {
                    id: author.into(),
                    name: author.into(),
                    is_bot: false,
                },
                content: String::new(),
            },
            args: String::new(),
            is_owner,
            prefix: "!".into(),
        }
    }

    fn sub(id: i64, created_by: &str) -> Subscription {
        Subscription {
            id,
            kind: AlertKind::YouTube,
            source_id: "UC1".into(),
            source_name: "n".into(),
            source_key: "UC1".into(),
            platform: Platform::Discord,
            guild_id: Some("g".into()),
            channel_id: "c".into(),
            include_shorts: false,
            created_by: created_by.into(),
        }
    }

    fn ids(subs: &[Subscription]) -> Vec<i64> {
        subs.iter().map(|s| s.id).collect()
    }

    #[tokio::test]
    async fn adder_changes_only_their_own() {
        let (ok, others) =
            permitted(&ctx("alice", false), vec![sub(1, "alice"), sub(2, "bob")]).await;
        assert_eq!(ids(&ok), [1]);
        assert_eq!(others, 1);
    }

    #[tokio::test]
    async fn others_get_nothing() {
        let (ok, others) = permitted(&ctx("mallory", false), vec![sub(1, "alice")]).await;
        assert!(ok.is_empty());
        assert_eq!(others, 1);
    }

    #[tokio::test]
    async fn server_owner_and_bot_owner_change_everything() {
        let all = || vec![sub(1, "alice"), sub(2, "bob")];
        let (ok, others) = permitted(&ctx("owner", false), all()).await;
        assert_eq!((ids(&ok), others), (vec![1, 2], 0));
        let (ok, others) = permitted(&ctx("botadmin", true), all()).await;
        assert_eq!((ids(&ok), others), (vec![1, 2], 0));
    }
}
