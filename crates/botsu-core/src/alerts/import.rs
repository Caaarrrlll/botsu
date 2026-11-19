//! Parsing pasted subscription lists from other bots (for example Red's Tube and streams cogs)
//! for `tube import` / `twitch import`.
//!
//! Lists can say which channel each group of subscriptions posts to:
//! - Red Tube (`tube list`): `Tube Subs for my-lives (111111111111111111)` → channel by ID
//! - Red streams (`streamalert list`): `#stream-alerts` (or a bare `stream-alerts`) → channel by name
//!
//! Entries before any channel heading go to the default channel (the one given in the command,
//! or where it was typed).

use super::{twitch, youtube};

/// Words that join names in lists ("a, b and c") rather than being names.
const CONNECTORS: &[&str] = &["and", "&", "-", "+", "|", "or"];
/// Lines that only label the platform in other bots' lists.
const LABELS: &[&str] = &[
    "twitch",
    "youtube",
    "youtubestream",
    "twitchstream",
    "picarto",
    "trovo",
    "active alerts",
];

/// Where a group of imported subscriptions should post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The channel given in the command, or the one it was typed in.
    Default,
    Id(String),
    /// A channel name to look up in the server (without `#`).
    Name(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Group<T> {
    pub target: Target,
    pub items: Vec<T>,
}

fn push<T: PartialEq>(groups: &mut Vec<Group<T>>, target: &Target, item: T) {
    if let Some(g) = groups.last_mut().filter(|g| g.target == *target) {
        if !g.items.contains(&item) {
            g.items.push(item);
        }
        return;
    }
    groups.push(Group {
        target: target.clone(),
        items: vec![item],
    });
}

/// Splits pasted text into `(section, line)` pairs. A line like `twitch:` or `YouTube:` starts a
/// section; lines before any heading belong to section `""`. `#channel` lines are never
/// section headings.
fn sections(text: &str) -> Vec<(String, &str)> {
    let mut current = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(head) = t.strip_suffix(':').filter(|_| !t.starts_with('#')) {
            if head.split_whitespace().count() <= 2 {
                current = head.trim().to_lowercase();
                continue;
            }
        }
        out.push((current.clone(), t));
    }
    out
}

/// Lines to read for one kind: the matching section if the paste has headings for it,
/// otherwise every line.
fn lines_for<'a>(text: &'a str, wanted: &[&str]) -> Vec<&'a str> {
    let all = sections(text);
    let matches = |s: &str| wanted.iter().any(|w| s.contains(w));
    if all.iter().any(|(s, _)| matches(s)) {
        all.into_iter()
            .filter(|(s, _)| matches(s))
            .map(|(_, l)| l)
            .collect()
    } else {
        all.into_iter().map(|(_, l)| l).collect()
    }
}

fn is_label(line: &str) -> bool {
    let l = line.trim().trim_end_matches(':').to_lowercase();
    LABELS.contains(&l.as_str())
}

/// A channel heading: `<#123>`, `#name`, or a line with a `(123456789012345678)` channel ID and
/// no YouTube channel ID (Red's `Tube Subs for name (id)`).
fn heading(line: &str) -> Option<Target> {
    let t = line.trim().trim_end_matches(':').trim();
    if let Some(id) = super::parse_channel(t) {
        return Some(Target::Id(id));
    }
    if let Some(name) = t.strip_prefix('#') {
        let name = name.trim();
        if !name.is_empty() && !name.contains(char::is_whitespace) {
            return Some(Target::Name(name.to_owned()));
        }
    }
    if find_channel_ids(t).is_empty() {
        let mut rest = t;
        while let Some(open) = rest.find('(') {
            let after = &rest[open + 1..];
            if let Some(close) = after.find(')') {
                let inner = &after[..close];
                if (15..=21).contains(&inner.len()) && inner.bytes().all(|b| b.is_ascii_digit()) {
                    return Some(Target::Id(inner.to_owned()));
                }
                rest = &after[close + 1..];
            } else {
                break;
            }
        }
    }
    None
}

fn is_id_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// `(start, id)` of every standalone `UC...` channel ID in a line.
fn find_channel_ids(line: &str) -> Vec<(usize, String)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = line[i..].find("UC") {
        let start = i + pos;
        i = start + 2;
        let Some(candidate) = line.get(start..start + 24) else {
            break;
        };
        let before_ok = start == 0 || !is_id_char(bytes[start - 1]);
        let after_ok = bytes.get(start + 24).is_none_or(|b| !is_id_char(*b));
        if youtube::is_channel_id(candidate) && before_ok && after_ok {
            out.push((start, candidate.to_owned()));
            i = start + 24;
        }
    }
    out
}

/// A YouTube channel to import: its ID and, if the line had one, the name in front of it.
#[derive(Debug, PartialEq, Eq)]
pub struct YouTubeEntry {
    pub channel_id: String,
    pub name: Option<String>,
}

/// Finds every `UC...` channel ID, e.g. in Red's `Name (UC...) - 2026-11-18T07:00:22+00:00`,
/// grouped by channel heading. Lines may also be channel links.
pub fn parse_youtube(text: &str) -> Vec<Group<YouTubeEntry>> {
    let mut groups = Vec::new();
    let mut target = Target::Default;
    for line in lines_for(text, &["youtube", "tube"]) {
        if let Some(t) = heading(line) {
            target = t;
            continue;
        }
        for (start, id) in find_channel_ids(line) {
            if groups
                .iter()
                .any(|g: &Group<YouTubeEntry>| g.items.iter().any(|e| e.channel_id == id))
            {
                continue;
            }
            let name = line[..start]
                .trim()
                .trim_end_matches(['(', '[', '-', ':', '|'])
                .trim()
                .trim_start_matches(['•', '*', '-'])
                .trim();
            push(
                &mut groups,
                &target,
                YouTubeEntry {
                    channel_id: id,
                    name: (!name.is_empty() && !name.contains("http")).then(|| name.to_owned()),
                },
            );
        }
    }
    groups
}

fn is_login_word(word: &str) -> bool {
    word.len() >= 3 && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Twitch logins from lines like `a and b`, `a, b`, `twitch.tv/a`, grouped by channel heading.
/// A line holding one word that can't be a Twitch name (like `news-feed`) is taken as a
/// channel name, as in Red's `streamalert list`. Returns `(groups, skipped words)`.
pub fn parse_twitch(text: &str) -> (Vec<Group<String>>, Vec<String>) {
    let mut groups = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut target = Target::Default;
    for line in lines_for(text, &["twitch", "stream"]) {
        if is_label(line) || !find_channel_ids(line).is_empty() || line.contains("youtube.com") {
            continue;
        }
        if let Some(t) = heading(line) {
            target = t;
            continue;
        }
        let words: Vec<&str> = line
            .split([',', ' ', '\t', ';'])
            .map(|w| {
                w.trim()
                    .trim_start_matches(['•', '*', '@'])
                    .trim_end_matches([':', '.'])
            })
            .filter(|w| !w.is_empty() && !CONNECTORS.contains(&w.to_lowercase().as_str()))
            .collect();
        // A lone word that isn't a valid Twitch name: a channel heading.
        if let [only] = words.as_slice() {
            if !is_login_word(only) && !only.contains("twitch.tv") {
                target = Target::Name(only.to_string());
                continue;
            }
        }
        for word in words {
            let valid = word.contains("twitch.tv") || is_login_word(word);
            match twitch::parse_login(word).filter(|_| valid) {
                Some(login) => push(&mut groups, &target, login),
                None if !skipped.iter().any(|s| s == word) => skipped.push(word.to_owned()),
                None => {}
            }
        }
    }
    (groups, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> Target {
        Target::Name(n.into())
    }

    /// The hand-trimmed paste from the issue.
    const PASTE: &str = "youtube:
Channel One (UCxxxxxxxxxxxxxxxxxxxx01) - 2026-11-18T07:00:22+00:00
Channel Two (UCxxxxxxxxxxxxxxxxxxxx02) - 2026-11-10T15:30:04+00:00
Channel Three (UCxxxxxxxxxxxxxxxxxxxx03) - 2026-11-13T16:54:28+00:00
Channel Four (UCxxxxxxxxxxxxxxxxxxxx04) - 2026-11-18T16:00:16+00:00

twitch:
streamer_one_ and streamer2
news-feed
streamer_three
streamer4";

    /// Red's full output, as copied from Discord.
    const RED_TUBE: &str = "Tube Subs for my-lives (111111111111111111)
Channel One (UCxxxxxxxxxxxxxxxxxxxx01) - 2026-11-18T07:00:22+00:00
Tube Subs for clips (222222222222222222)
Channel Two (UCxxxxxxxxxxxxxxxxxxxx02) - 2026-11-10T15:30:04+00:00
Channel Three (UCxxxxxxxxxxxxxxxxxxxx03) - 2026-11-13T16:54:28+00:00
Channel Four (UCxxxxxxxxxxxxxxxxxxxx04) - 2026-11-18T16:00:16+00:00";

    const RED_STREAMS: &str = "Active alerts:

#stream-alerts
Twitch
streamer_one_ and streamer2
#news-feed
Twitch
streamer_three
#clips
Twitch
streamer4";

    #[test]
    fn red_tube_list_keeps_channels() {
        let groups = parse_youtube(RED_TUBE);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].target, Target::Id("111111111111111111".into()));
        assert_eq!(groups[0].items.len(), 1);
        assert_eq!(
            groups[0].items[0],
            YouTubeEntry {
                channel_id: "UCxxxxxxxxxxxxxxxxxxxx01".into(),
                name: Some("Channel One".into())
            }
        );
        assert_eq!(groups[1].target, Target::Id("222222222222222222".into()));
        let names: Vec<_> = groups[1]
            .items
            .iter()
            .map(|e| e.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["Channel Two", "Channel Three", "Channel Four"]);
    }

    #[test]
    fn red_streams_list_keeps_channels() {
        let (groups, skipped) = parse_twitch(RED_STREAMS);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(
            groups,
            vec![
                Group {
                    target: name("stream-alerts"),
                    items: vec!["streamer_one_".into(), "streamer2".into()]
                },
                Group {
                    target: name("news-feed"),
                    items: vec!["streamer_three".into()]
                },
                Group {
                    target: name("clips"),
                    items: vec!["streamer4".into()]
                },
            ]
        );
    }

    #[test]
    fn trimmed_paste_still_works() {
        let yt = parse_youtube(PASTE);
        assert_eq!(yt.len(), 1);
        assert_eq!(yt[0].target, Target::Default);
        assert_eq!(yt[0].items.len(), 4);

        // Without '#', a lone non-Twitch word still becomes the channel for what follows.
        let (tw, skipped) = parse_twitch(PASTE);
        assert!(skipped.is_empty());
        assert_eq!(tw[0].target, Target::Default);
        assert_eq!(tw[0].items, ["streamer_one_", "streamer2"]);
        assert_eq!(tw[1].target, name("news-feed"));
        assert_eq!(tw[1].items, ["streamer_three", "streamer4"]);
    }

    #[test]
    fn both_lists_in_one_paste_do_not_mix() {
        let both = format!("{RED_TUBE}\n{RED_STREAMS}");
        let yt = parse_youtube(&both);
        assert_eq!(yt.iter().map(|g| g.items.len()).sum::<usize>(), 4);
        let (tw, skipped) = parse_twitch(&both);
        assert_eq!(tw.iter().map(|g| g.items.len()).sum::<usize>(), 4, "{tw:?}");
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    #[test]
    fn plain_lists() {
        let (tw, skipped) = parse_twitch("a_b, cde and https://twitch.tv/Fgh\n");
        assert_eq!(tw[0].items, ["a_b", "cde", "fgh"]);
        assert!(skipped.is_empty());

        let yt = parse_youtube("https://www.youtube.com/channel/UCxxxxxxxxxxxxxxxxxxxx02\n");
        assert_eq!(yt[0].items[0].name, None);
        assert_eq!(
            parse_youtube(
                "UCxxxxxxxxxxxxxxxxxxxx01 UCxxxxxxxxxxxxxxxxxxxx01 UCxxxxxxxxxxxxxxxxxxxx01X"
            )[0]
            .items
            .len(),
            1
        );
    }
}
