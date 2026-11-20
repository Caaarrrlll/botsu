//! Alerts: YouTube uploads (Red's Tube cog) and Twitch live streams (Red's streams cog).
//!
//! Subscriptions are stored per target channel. Each source (YouTube channel, Twitch user) is
//! polled once no matter how many channels subscribe to it, and each item (video, stream) is
//! recorded as seen so it's posted once, also across restarts.

mod commands;
pub mod import;
pub mod twitch;
pub mod youtube;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;

pub use commands::{TubeCommand, TwitchCommand};

use crate::bot::Bot;
use crate::storage::{now, AlertKind, PendingItem, Store, Subscription};

/// Uploads older than this are never posted, even if unseen (for example a video made public
/// long after upload, or after the seen-items table was pruned).
const MAX_VIDEO_AGE: time::Duration = time::Duration::days(7);
/// A new Twitch stream starting within this long after the previous one was last seen live is
/// treated as a reconnect of the same broadcast and not announced again.
const STREAM_RESTART_GRACE_SECS: i64 = 15 * 60;
/// Seen items are kept this long. Must stay well above MAX_VIDEO_AGE.
const SEEN_RETENTION_SECS: i64 = 365 * 24 * 3600;
/// Marker recorded at subscribe time, so a source's first real item counts as new.
pub(crate) const BASELINE: &str = "__baseline__";

#[derive(Debug, Clone)]
pub struct AlertSettings {
    pub youtube_interval: Duration,
    pub twitch_interval: Duration,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self {
            youtube_interval: Duration::from_secs(300),
            twitch_interval: Duration::from_secs(120),
        }
    }
}

/// Starts the background pollers. Call after the adapters have been spawned; the first poll
/// waits a little so they have time to connect.
pub fn spawn_pollers(
    bot: Arc<Bot>,
    store: Arc<Store>,
    http: reqwest::Client,
    youtube_api: Option<Arc<youtube::YouTubeApi>>,
    twitch: Option<Arc<twitch::Twitch>>,
    settings: AlertSettings,
) {
    {
        let (bot, store) = (bot.clone(), store.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(15)).await;
            loop {
                if let Err(e) = poll_youtube(&bot, &store, &http, youtube_api.as_deref()).await {
                    tracing::error!(error = %e, "YouTube poll failed");
                }
                if let Err(e) = store.prune_seen(SEEN_RETENTION_SECS) {
                    tracing::warn!(error = %e, "pruning seen items failed");
                }
                tokio::time::sleep(jitter(settings.youtube_interval)).await;
            }
        });
    }
    if let Some(twitch) = twitch {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(15)).await;
            loop {
                if let Err(e) = poll_twitch(&bot, &store, &twitch).await {
                    tracing::error!(error = %e, "Twitch poll failed");
                }
                tokio::time::sleep(jitter(settings.twitch_interval)).await;
            }
        });
    }
}

/// ±10% so the bot doesn't hit YouTube at exactly the same second every cycle.
fn jitter(base: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let factor = 0.9 + (nanos % 1000) as f64 / 5000.0; // 0.9 ..= 1.1
    base.mul_f64(factor)
}

fn group_by_source(subs: Vec<Subscription>) -> BTreeMap<String, Vec<Subscription>> {
    let mut map: BTreeMap<String, Vec<Subscription>> = BTreeMap::new();
    for s in subs {
        map.entry(s.source_id.clone()).or_default().push(s);
    }
    map
}

/// A feed item not seen before, waiting to be classified and posted this cycle.
struct NewItem {
    source_id: String,
    channel_name: String,
    video: youtube::Video,
}

/// Scheduled streams that never go live are dropped after this long.
const PENDING_MAX_AGE_SECS: i64 = 14 * 24 * 3600;

async fn poll_youtube(
    bot: &Bot,
    store: &Store,
    http: &reqwest::Client,
    api: Option<&youtube::YouTubeApi>,
) -> Result<(), String> {
    let db = |e: rusqlite::Error| e.to_string();
    let sources = group_by_source(store.all_subscriptions(AlertKind::YouTube).map_err(db)?);
    tracing::debug!(sources = sources.len(), "polling YouTube");

    // 1. New items from every feed (oldest first, so several uploads post in order).
    let cutoff = OffsetDateTime::now_utc() - MAX_VIDEO_AGE;
    let mut new_items = Vec::new();
    for source_id in sources.keys() {
        let feed = match youtube::fetch_feed(http, source_id).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(channel = %source_id, error = %e, "YouTube feed failed");
                continue;
            }
        };
        let first_time = !store
            .has_seen_any(AlertKind::YouTube, source_id)
            .map_err(db)?;
        for video in feed.videos.into_iter().rev() {
            if store
                .is_seen(AlertKind::YouTube, source_id, &video.id)
                .map_err(db)?
            {
                continue;
            }
            store
                .mark_seen(AlertKind::YouTube, source_id, &video.id)
                .map_err(db)?;
            if first_time || video.published.is_some_and(|p| p < cutoff) {
                continue;
            }
            new_items.push(NewItem {
                source_id: source_id.clone(),
                channel_name: feed.channel_name.clone(),
                video,
            });
        }
        // Be gentle with YouTube when there are many channels.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // 2. Ask the Data API what the new (non-Short) items and the waiting ones are. One request
    //    per 50 videos. Shorts can't be live, so they're never sent.
    let pending = store.pending(AlertKind::YouTube).map_err(db)?;
    let classified: Option<Result<BTreeMap<String, youtube::VideoInfo>, String>> = match api {
        None => None,
        Some(api) => {
            let ids: Vec<String> = new_items
                .iter()
                .filter(|i| !i.video.is_short)
                .map(|i| i.video.id.clone())
                .chain(pending.iter().map(|p| p.item_id.clone()))
                .collect();
            Some(if ids.is_empty() {
                Ok(BTreeMap::new())
            } else {
                api.videos(&ids)
                    .await
                    .map(|v| v.into_iter().map(|i| (i.id.clone(), i)).collect())
            })
        }
    };
    if let Some(Err(e)) = &classified {
        tracing::warn!(error = %e, "YouTube API check failed; new items will be retried next poll");
    }

    // 3. Post new items.
    for item in new_items {
        let Some(subs) = sources.get(&item.source_id) else {
            continue;
        };
        let (ch, v) = (&item.channel_name, &item.video);
        if v.is_short {
            let wanted: Vec<Subscription> =
                subs.iter().filter(|s| s.include_shorts).cloned().collect();
            if !wanted.is_empty() {
                tracing::info!(channel = %ch, video = %v.id, "new YouTube Short");
                let alert = Alert::new(Event::Short, ch, &v.title, &v.url);
                deliver(bot, &wanted, &alert).await;
            }
            continue;
        }
        let state = match &classified {
            None => Some(youtube::LiveState::None), // no API key: everything is an upload
            Some(Ok(map)) => match map.get(&v.id) {
                Some(info) => Some(info.state),
                None => continue, // private or deleted already
            },
            Some(Err(_)) => None, // classify next poll
        };
        match state {
            Some(youtube::LiveState::None) => {
                tracing::info!(channel = %ch, video = %v.id, "new YouTube upload");
                let alert = Alert::new(Event::Upload, ch, &v.title, &v.url);
                deliver(bot, subs, &alert).await;
            }
            Some(youtube::LiveState::Live) => {
                tracing::info!(channel = %ch, video = %v.id, "YouTube stream is live");
                let alert = Alert::new(Event::YouTubeLive, ch, &v.title, &v.url);
                deliver(bot, subs, &alert).await;
            }
            Some(youtube::LiveState::Upcoming) | None => {
                tracing::info!(channel = %ch, video = %v.id, "YouTube item waiting (scheduled, or API unavailable)");
                store
                    .add_pending(
                        AlertKind::YouTube,
                        &PendingItem {
                            source_id: item.source_id.clone(),
                            source_name: ch.clone(),
                            item_id: v.id.clone(),
                            title: v.title.clone(),
                            url: v.url.clone(),
                            added_at: now(),
                        },
                    )
                    .map_err(db)?;
            }
        }
    }

    // 4. Waiting items: post when they go live, drop when gone or too old.
    let Some(Ok(map)) = &classified else {
        return Ok(());
    };
    for p in pending {
        let subs = sources.get(&p.source_id);
        let expired = now() - p.added_at > PENDING_MAX_AGE_SECS;
        let event = match map.get(&p.item_id).map(|i| (i.state, i.started)) {
            Some((youtube::LiveState::Upcoming, _)) if !expired && subs.is_some() => continue,
            Some((youtube::LiveState::Live, _)) => Some(Event::YouTubeLive),
            // Went live and ended between two polls.
            Some((youtube::LiveState::None, true)) => Some(Event::YouTubeWasLive),
            // Waited only because the API was down: a normal upload.
            Some((youtube::LiveState::None, false)) => Some(Event::Upload),
            // Deleted/private, expired, or nobody subscribes any more.
            _ => None,
        };
        store
            .remove_pending(AlertKind::YouTube, &p.item_id)
            .map_err(db)?;
        if let (Some(event), Some(subs)) = (event, subs) {
            tracing::info!(channel = %p.source_name, video = %p.item_id, "waiting YouTube item posted");
            deliver(
                bot,
                subs,
                &Alert::new(event, &p.source_name, &p.title, &p.url),
            )
            .await;
        }
    }
    Ok(())
}

/// What happened, for the default text and the `{event}` placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    Upload,
    Short,
    YouTubeLive,
    YouTubeWasLive,
    TwitchLive,
}

impl Event {
    fn phrase(self) -> &'static str {
        match self {
            Event::Upload => "posted a new video",
            Event::Short => "posted a new Short",
            Event::YouTubeLive => "is live on YouTube",
            Event::YouTubeWasLive => "was live on YouTube",
            Event::TwitchLive => "is live on Twitch",
        }
    }
}

/// One alert, rendered per subscription: its custom text (if set) and mention.
pub(crate) struct Alert {
    pub event: Event,
    pub name: String,
    pub title: String,
    pub url: String,
    /// Twitch category; empty for YouTube.
    pub game: String,
}

/// Placeholders a custom alert text may use (`tube message`, `twitch message`).
pub const PLACEHOLDERS: &str = "`{name}`, `{title}`, `{url}`, `{game}` (Twitch), `{event}`";

impl Alert {
    pub(crate) fn new(event: Event, name: &str, title: &str, url: &str) -> Self {
        Self {
            event,
            name: name.to_owned(),
            title: title.to_owned(),
            url: url.to_owned(),
            game: String::new(),
        }
    }

    pub(crate) fn twitch(s: &twitch::Stream) -> Self {
        Self {
            event: Event::TwitchLive,
            name: s.user_name.clone(),
            title: s.title.clone(),
            url: format!("https://twitch.tv/{}", s.user_login),
            game: s.game_name.clone(),
        }
    }

    /// The built-in text.
    pub(crate) fn default_text(&self) -> String {
        let (name, event) = (&self.name, self.event.phrase());
        if self.event == Event::TwitchLive {
            let mut text = format!("**{name}** {event}");
            if !self.game.is_empty() {
                text.push_str(&format!(" playing **{}**", self.game));
            }
            if !self.title.is_empty() {
                text.push_str(&format!(": {}", self.title));
            }
            return format!("{text}\n{}", self.url);
        }
        format!("**{name}** {event}: **{}**\n{}", self.title, self.url)
    }

    /// The text for one subscription: its template with the placeholders filled in (the link
    /// is added on its own line if the template leaves it out, so the preview still shows), or
    /// the default; then the mention in front.
    pub(crate) fn text_for(&self, message: Option<&str>, mention: Option<&str>) -> String {
        let body = match message {
            Some(t) => {
                let filled = t
                    .replace("{name}", &self.name)
                    .replace("{title}", &self.title)
                    .replace("{game}", &self.game)
                    .replace("{event}", self.event.phrase());
                if filled.contains("{url}") {
                    filled.replace("{url}", &self.url)
                } else {
                    format!("{filled}\n{}", self.url)
                }
            }
            None => self.default_text(),
        };
        match mention.map(mention_text) {
            Some(m) => format!("{m} {body}"),
            None => body,
        }
    }
}

/// A stored mention (role ID, `everyone`, `here`) as message text.
pub(crate) fn mention_text(mention: &str) -> String {
    match mention {
        "everyone" => "@everyone".to_owned(),
        "here" => "@here".to_owned(),
        role => format!("<@&{role}>"),
    }
}

async fn poll_twitch(bot: &Bot, store: &Store, twitch: &twitch::Twitch) -> Result<(), String> {
    let sources = group_by_source(
        store
            .all_subscriptions(AlertKind::Twitch)
            .map_err(|e| e.to_string())?,
    );
    if sources.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = sources.keys().cloned().collect();
    let live = twitch.live_streams(&ids).await?;
    tracing::debug!(sources = ids.len(), live = live.len(), "polled Twitch");

    let now = now();
    for stream in live {
        let Some(subs) = sources.get(&stream.user_id) else {
            continue;
        };
        let previous = store
            .last_live_at(AlertKind::Twitch, &stream.user_id)
            .map_err(|e| e.to_string())?;
        store
            .set_last_live_at(AlertKind::Twitch, &stream.user_id, now)
            .map_err(|e| e.to_string())?;
        if store
            .is_seen(AlertKind::Twitch, &stream.user_id, &stream.id)
            .map_err(|e| e.to_string())?
        {
            continue;
        }
        store
            .mark_seen(AlertKind::Twitch, &stream.user_id, &stream.id)
            .map_err(|e| e.to_string())?;
        if previous.is_some_and(|p| now - p < STREAM_RESTART_GRACE_SECS) {
            tracing::info!(user = %stream.user_login, "stream restarted shortly after going offline; not announcing again");
            continue;
        }
        tracing::info!(user = %stream.user_login, stream = %stream.id, "Twitch stream went live");
        deliver(bot, subs, &Alert::twitch(&stream)).await;
    }
    Ok(())
}

pub(crate) fn live_text(s: &twitch::Stream) -> String {
    Alert::twitch(s).default_text()
}

/// Posts the alert to every subscribed channel, with each subscription's own text and mention.
/// Failures are logged, not retried: a channel the bot can't post in shouldn't block the others
/// or cause repeats later.
async fn deliver(bot: &Bot, subs: &[Subscription], alert: &Alert) {
    for sub in subs {
        let text = alert.text_for(sub.message.as_deref(), sub.mention.as_deref());
        let text = text.as_str();
        let Some(chat) = bot.chat(sub.platform) else {
            tracing::warn!(platform = %sub.platform, "platform not connected; alert skipped");
            continue;
        };
        if let Err(e) = chat.send_message(&sub.channel_id, text).await {
            tracing::warn!(
                platform = %sub.platform,
                channel = %sub.channel_id,
                subscription = sub.id,
                error = %e,
                "couldn't post alert"
            );
        }
    }
}

/// A target channel argument: a `<#id>` mention or a bare numeric ID.
pub fn parse_channel(arg: &str) -> Option<String> {
    let id = arg
        .strip_prefix("<#")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(arg);
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_args() {
        assert_eq!(parse_channel("<#123>").as_deref(), Some("123"));
        assert_eq!(parse_channel("456").as_deref(), Some("456"));
        assert_eq!(parse_channel("#general"), None);
        assert_eq!(parse_channel("<#abc>"), None);
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        let d = jitter(Duration::from_secs(100));
        assert!(d >= Duration::from_secs(90) && d <= Duration::from_secs(110));
    }

    #[test]
    fn alert_texts() {
        let a = Alert::new(Event::Upload, "Chan", "Vid", "https://y/1");
        assert_eq!(
            a.text_for(None, None),
            "**Chan** posted a new video: **Vid**\nhttps://y/1"
        );
        assert_eq!(
            a.text_for(Some("{name} {event}: {title} {url}"), Some("123")),
            "<@&123> Chan posted a new video: Vid https://y/1"
        );
        // No {url}: the link goes on its own line so the preview still shows.
        assert_eq!(
            a.text_for(Some("New one from {name}!"), Some("everyone")),
            "@everyone New one from {name}!".replace("{name}", "Chan") + "\nhttps://y/1"
        );
        assert_eq!(
            Alert::new(Event::YouTubeWasLive, "C", "T", "u").text_for(None, Some("here")),
            "@here **C** was live on YouTube: **T**\nu"
        );
    }

    #[test]
    fn live_text_skips_empty_fields() {
        let s = twitch::Stream {
            id: "1".into(),
            user_id: "2".into(),
            user_login: "abc".into(),
            user_name: "Abc".into(),
            game_name: String::new(),
            title: "Hello".into(),
        };
        assert_eq!(
            live_text(&s),
            "**Abc** is live on Twitch: Hello\nhttps://twitch.tv/abc"
        );
    }
}
