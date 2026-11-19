//! YouTube alerts.
//!
//! - **New items** come from the official RSS feed
//!   `https://www.youtube.com/feeds/videos.xml?channel_id=UC...`: no API key or quota needed. It
//!   lists the ~15 newest items (videos, Shorts, streams, premieres). YouTube caches it, so a new
//!   upload can take up to ~15 minutes to appear. Shorts are linked as `/shorts/<id>`.
//! - **What an item is** (live stream, upcoming stream/premiere, regular upload) and **@handle
//!   lookups** use the YouTube Data API v3 with `YOUTUBE_API_KEY`. Each poll costs 1 quota unit
//!   per 50 new or pending items, far below the free 10,000 units per day. Without a key, every
//!   new item is posted as an upload and only channel IDs/links are accepted.

use serde::Deserialize;
use time::OffsetDateTime;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const YT: &str = "http://www.youtube.com/xml/schemas/2015";
const API: &str = "https://www.googleapis.com/youtube/v3";

#[derive(Debug, Clone)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub url: String,
    pub published: Option<OffsetDateTime>,
    pub is_short: bool,
}

#[derive(Debug, Clone)]
pub struct Feed {
    pub channel_name: String,
    /// Newest first, as YouTube sends them.
    pub videos: Vec<Video>,
}

pub fn feed_url(channel_id: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}")
}

pub fn is_channel_id(s: &str) -> bool {
    s.len() == 24
        && s.starts_with("UC")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub async fn fetch_feed(http: &reqwest::Client, channel_id: &str) -> Result<Feed, String> {
    let resp = http
        .get(feed_url(channel_id))
        .send()
        .await
        .map_err(|e| format!("feed request failed: {e}"))?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Err("YouTube has no feed for that channel ID".into()),
        s => return Err(format!("feed returned HTTP {s}")),
    }
    let body = resp.text().await.map_err(|e| format!("feed body: {e}"))?;
    parse_feed(&body)
}

pub fn parse_feed(xml: &str) -> Result<Feed, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("feed is not valid XML: {e}"))?;
    let root = doc.root_element();
    let child_text = |node: roxmltree::Node<'_, '_>, ns: &str, name: &str| -> Option<String> {
        node.children()
            .find(|c| c.has_tag_name((ns, name)))
            .and_then(|c| c.text())
            .map(|t| t.trim().to_owned())
    };

    let channel_name = child_text(root, ATOM, "title").unwrap_or_default();
    let videos = root
        .children()
        .filter(|n| n.has_tag_name((ATOM, "entry")))
        .filter_map(|entry| {
            let id = child_text(entry, YT, "videoId")?;
            let url = entry
                .children()
                .find(|c| c.has_tag_name((ATOM, "link")) && c.attribute("rel") == Some("alternate"))
                .and_then(|c| c.attribute("href"))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={id}"));
            let published = child_text(entry, ATOM, "published").and_then(|p| {
                OffsetDateTime::parse(&p, &time::format_description::well_known::Rfc3339).ok()
            });
            Some(Video {
                title: child_text(entry, ATOM, "title").unwrap_or_else(|| id.clone()),
                is_short: url.contains("/shorts/"),
                id,
                url,
                published,
            })
        })
        .collect();
    Ok(Feed {
        channel_name,
        videos,
    })
}

// --- Data API -----------------------------------------------------------------------------

/// What the API says a video currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    /// A regular upload, or a stream/premiere that has ended.
    None,
    /// Scheduled stream or premiere that hasn't started.
    Upcoming,
    Live,
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub id: String,
    pub state: LiveState,
    /// The item was a stream or premiere that actually started (even if it has ended).
    pub started: bool,
}

#[derive(Deserialize)]
struct ApiList<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
}

#[derive(Deserialize)]
struct ApiId {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiVideo {
    id: String,
    snippet: ApiSnippet,
    live_streaming_details: Option<ApiLiveDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiSnippet {
    #[serde(default)]
    live_broadcast_content: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiLiveDetails {
    actual_start_time: Option<String>,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    error: ApiError,
}

#[derive(Deserialize)]
struct ApiError {
    message: String,
}

pub struct YouTubeApi {
    http: reqwest::Client,
    key: String,
}

impl YouTubeApi {
    pub fn new(http: reqwest::Client, key: String) -> Self {
        Self { http, key }
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        resource: &str,
        query: &[(&str, &str)],
    ) -> Result<T, String> {
        let resp = self
            .http
            .get(format!("{API}/{resource}"))
            .query(query)
            .query(&[("key", self.key.as_str())])
            .send()
            .await
            .map_err(|e| format!("YouTube API request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            // The API explains itself (invalid key, quota exceeded, API not enabled, ...).
            let why = resp
                .json::<ApiErrorBody>()
                .await
                .map(|b| b.error.message)
                .unwrap_or_default();
            return Err(format!("YouTube API returned HTTP {status}: {why}"));
        }
        resp.json()
            .await
            .map_err(|e| format!("YouTube API response: {e}"))
    }

    /// Checks the key works (1 quota unit); used at startup for a clear error.
    pub async fn check(&self) -> Result<(), String> {
        self.get::<ApiList<ApiId>>(
            "channels",
            &[("part", "id"), ("id", "UCBR8-60-B28hp2BmDPdntcQ")],
        )
        .await
        .map(|_| ())
    }

    /// `@handle` → channel ID.
    pub async fn channel_by_handle(&self, handle: &str) -> Result<Option<String>, String> {
        let handle = format!("@{}", handle.trim_start_matches('@'));
        let list: ApiList<ApiId> = self
            .get("channels", &[("part", "id"), ("forHandle", &handle)])
            .await?;
        Ok(list.items.into_iter().next().map(|c| c.id))
    }

    /// Legacy `youtube.com/user/<name>` → channel ID.
    pub async fn channel_by_username(&self, name: &str) -> Result<Option<String>, String> {
        let list: ApiList<ApiId> = self
            .get("channels", &[("part", "id"), ("forUsername", name)])
            .await?;
        Ok(list.items.into_iter().next().map(|c| c.id))
    }

    /// Live state of up to any number of videos (50 per request). Deleted or private videos are
    /// absent from the result.
    pub async fn videos(&self, ids: &[String]) -> Result<Vec<VideoInfo>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(50) {
            let joined = chunk.join(",");
            let list: ApiList<ApiVideo> = self
                .get(
                    "videos",
                    &[("part", "snippet,liveStreamingDetails"), ("id", &joined)],
                )
                .await?;
            out.extend(list.items.into_iter().map(video_info));
        }
        Ok(out)
    }
}

fn video_info(v: ApiVideo) -> VideoInfo {
    let state = match v.snippet.live_broadcast_content.as_str() {
        "live" => LiveState::Live,
        "upcoming" => LiveState::Upcoming,
        _ => LiveState::None,
    };
    let started = v
        .live_streaming_details
        .is_some_and(|d| d.actual_start_time.is_some());
    VideoInfo {
        id: v.id,
        state,
        started,
    }
}

// --- Resolving what the user typed ----------------------------------------------------------

/// What the user typed, cleaned up: Discord-style `<url>` brackets removed.
fn clean(input: &str) -> &str {
    input.trim().trim_start_matches('<').trim_end_matches('>')
}

/// Turns a channel ID, channel link, `@handle` or handle link into a channel ID.
/// Returns `(channel_id, key)` where `key` is the handle when one was given (used for removal).
/// Handles and `/user/` links need the Data API.
pub async fn resolve_channel(
    api: Option<&YouTubeApi>,
    input: &str,
) -> Result<(String, String), String> {
    let input = clean(input);
    if is_channel_id(input) {
        return Ok((input.to_owned(), input.to_owned()));
    }
    if let Some(id) = id_after(input, "/channel/") {
        return Ok((id.clone(), id));
    }

    let is_url = input.contains("youtube.com/") || input.contains("youtu.be/");
    let segment_after = |marker: &str| {
        input
            .split_once(marker)
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or("").to_owned())
            .filter(|s| !s.is_empty())
    };
    let (handle, username) = if is_url {
        (segment_after("/@"), segment_after("/user/"))
    } else if input.contains(char::is_whitespace) || input.trim_start_matches('@').is_empty() {
        return Err(format!(
            "`{input}` isn't a channel link, @handle or channel ID"
        ));
    } else {
        (Some(input.trim_start_matches('@').to_owned()), None)
    };

    let need_api = || {
        "looking up @handles needs YOUTUBE_API_KEY on the bot; use the channel's link \
         (youtube.com/channel/UC...) or its UC... ID instead"
            .to_owned()
    };
    if let Some(handle) = handle {
        let api = api.ok_or_else(need_api)?;
        return match api.channel_by_handle(&handle).await? {
            Some(id) => Ok((id, format!("@{handle}"))),
            None => Err(format!("YouTube has no channel @{handle}")),
        };
    }
    if let Some(name) = username {
        let api = api.ok_or_else(need_api)?;
        return match api.channel_by_username(&name).await? {
            Some(id) => Ok((id, name)),
            None => Err(format!("YouTube has no channel /user/{name}")),
        };
    }
    Err(format!(
        "can't tell which channel `{input}` is; use the channel's link or its UC... ID \
         (video links and /c/ links aren't supported)"
    ))
}

/// The channel ID that directly follows `marker` in `text`, if valid.
fn id_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let candidate = text.get(start..start + 24)?;
    is_channel_id(candidate).then(|| candidate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
 <link rel="self" href="http://www.youtube.com/feeds/videos.xml?channel_id=UCabcdefghijklmnopqrstuv"/>
 <id>yt:channel:abcdefghijklmnopqrstuv</id>
 <yt:channelId>abcdefghijklmnopqrstuv</yt:channelId>
 <title>Some Channel</title>
 <entry>
  <id>yt:video:vid2</id>
  <yt:videoId>vid2</yt:videoId>
  <title>Newest &amp; best</title>
  <link rel="alternate" href="https://www.youtube.com/shorts/vid2"/>
  <published>2026-11-18T10:00:00+00:00</published>
 </entry>
 <entry>
  <id>yt:video:vid1</id>
  <yt:videoId>vid1</yt:videoId>
  <title>Older</title>
  <link rel="alternate" href="https://www.youtube.com/watch?v=vid1"/>
  <published>2026-11-01T10:00:00+00:00</published>
 </entry>
</feed>"#;

    #[test]
    fn parses_feed_and_spots_shorts() {
        let feed = parse_feed(SAMPLE).unwrap();
        assert_eq!(feed.channel_name, "Some Channel");
        assert_eq!(feed.videos.len(), 2);
        assert_eq!(feed.videos[0].id, "vid2");
        assert_eq!(feed.videos[0].title, "Newest & best");
        assert!(feed.videos[0].is_short);
        assert!(!feed.videos[1].is_short);
        assert!(feed.videos[0].published.is_some());
    }

    #[test]
    fn reads_live_state_from_api_json() {
        let json = r#"{"items":[
            {"id":"a","snippet":{"liveBroadcastContent":"live"},"liveStreamingDetails":{"actualStartTime":"2026-11-19T10:00:00Z"}},
            {"id":"b","snippet":{"liveBroadcastContent":"upcoming"},"liveStreamingDetails":{"scheduledStartTime":"2026-11-20T10:00:00Z"}},
            {"id":"c","snippet":{"liveBroadcastContent":"none"}},
            {"id":"d","snippet":{"liveBroadcastContent":"none"},"liveStreamingDetails":{"actualStartTime":"x","actualEndTime":"y"}}
        ]}"#;
        let list: ApiList<ApiVideo> = serde_json::from_str(json).unwrap();
        let infos: Vec<VideoInfo> = list.items.into_iter().map(video_info).collect();
        assert_eq!(infos[0].state, LiveState::Live);
        assert_eq!(infos[1].state, LiveState::Upcoming);
        assert!(!infos[1].started);
        assert_eq!(infos[2].state, LiveState::None);
        assert!(!infos[2].started);
        assert!(infos[3].started);
    }

    #[tokio::test]
    async fn resolves_ids_and_links_without_api() {
        let id = "UCabcdefghijklmnopqrstuv";
        assert_eq!(resolve_channel(None, id).await.unwrap().0, id);
        let link = format!("<https://www.youtube.com/channel/{id}/videos>");
        assert_eq!(resolve_channel(None, &link).await.unwrap().0, id);
        let err = resolve_channel(None, "@someone").await.unwrap_err();
        assert!(err.contains("YOUTUBE_API_KEY"), "{err}");
        assert!(resolve_channel(None, "two words").await.is_err());
    }

    /// Hits the real YouTube feed. Run with `cargo test -p botsu-core -- --ignored`.
    #[tokio::test]
    #[ignore = "network"]
    async fn live_feed() {
        let http = reqwest::Client::new();
        let feed = fetch_feed(&http, "UCBR8-60-B28hp2BmDPdntcQ").await.unwrap();
        assert!(!feed.channel_name.is_empty());
        assert!(!feed.videos.is_empty());
        let shorts = feed.videos.iter().filter(|v| v.is_short).count();
        println!(
            "{}: {} items, {shorts} Shorts",
            feed.channel_name,
            feed.videos.len()
        );
    }

    /// Hits the real Data API. Run with YOUTUBE_API_KEY set and `-- --ignored`.
    #[tokio::test]
    #[ignore = "network + YOUTUBE_API_KEY"]
    async fn live_api() {
        let Ok(key) = std::env::var("YOUTUBE_API_KEY") else {
            return;
        };
        let api = YouTubeApi::new(reqwest::Client::new(), key);
        api.check().await.unwrap();
        let (id, _) = resolve_channel(Some(&api), "@YouTube").await.unwrap();
        assert_eq!(id, "UCBR8-60-B28hp2BmDPdntcQ");
    }

    #[test]
    fn finds_channel_ids() {
        assert!(is_channel_id("UCabcdefghijklmnopqrstuv"));
        assert!(!is_channel_id("UCshort"));
        assert_eq!(
            id_after(
                "https://www.youtube.com/channel/UCabcdefghijklmnopqrstuv/videos",
                "/channel/"
            )
            .as_deref(),
            Some("UCabcdefghijklmnopqrstuv")
        );
    }
}
