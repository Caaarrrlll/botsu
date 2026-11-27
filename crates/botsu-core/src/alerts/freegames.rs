//! Free games (keep forever) on Epic, Steam and GOG, from the GamerPower API
//! (https://www.gamerpower.com/api-read). Free, no key. One request per followed store per poll.

use serde::Deserialize;

const API: &str = "https://www.gamerpower.com/api/giveaways";

/// A store free games can be followed for. Its `key` is the subscription's `source_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shop {
    Epic,
    Steam,
    Gog,
}

impl Shop {
    pub const ALL: [Shop; 3] = [Shop::Epic, Shop::Steam, Shop::Gog];

    pub fn key(self) -> &'static str {
        match self {
            Shop::Epic => "epic",
            Shop::Steam => "steam",
            Shop::Gog => "gog",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Shop::Epic => "Epic Games Store",
            Shop::Steam => "Steam",
            Shop::Gog => "GOG",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    /// What people type: `epic`, `egs`, `epicgames`, `steam`, `gog` (any case).
    pub fn parse(word: &str) -> Option<Self> {
        match word.to_lowercase().replace(['-', '_', ' '], "").as_str() {
            "epic" | "egs" | "epicgames" | "epicgamesstore" => Some(Shop::Epic),
            "steam" => Some(Shop::Steam),
            "gog" | "gogcom" => Some(Shop::Gog),
            _ => None,
        }
    }

    /// The colour bar of the embed.
    pub fn color(self) -> u32 {
        match self {
            Shop::Epic => 0x0078F2,
            Shop::Steam => 0x1B2838,
            Shop::Gog => 0x86328A,
        }
    }

    fn api_platform(self) -> &'static str {
        match self {
            Shop::Epic => "epic-games-store",
            Shop::Steam => "steam",
            Shop::Gog => "gog",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Giveaway {
    pub id: u64,
    pub title: String,
    /// Normal price, like `$14.99`; `N/A` when unknown.
    #[serde(default)]
    pub worth: String,
    /// Large picture (GamerPower's copy of the store art).
    #[serde(default)]
    pub image: String,
    /// Link that redirects to the store page.
    pub open_giveaway_url: String,
    /// `2026-12-03 23:59:00`, or `N/A` when there's no end date.
    #[serde(default)]
    pub end_date: String,
}

impl Giveaway {
    /// The game's name without GamerPower's wording: `Bread and Fred (Epic Games) Giveaway`
    /// gives `Bread and Fred`.
    pub fn game(&self) -> String {
        let mut t = self.title.trim();
        t = t.strip_suffix(" Giveaway").unwrap_or(t).trim_end();
        t = t.strip_suffix(" Key").unwrap_or(t).trim_end();
        t = t.strip_suffix(" Steam").unwrap_or(t).trim_end();
        if t.ends_with(')') {
            if let Some(open) = t.rfind(" (") {
                let inside = t[open + 2..t.len() - 1].to_lowercase();
                if ["epic", "steam", "gog"].iter().any(|s| inside.contains(s)) {
                    t = &t[..open];
                }
            }
        }
        t.to_owned()
    }

    /// `~~$14.99~~ FREE`, or just `FREE` when the price isn't known.
    pub fn price(&self) -> String {
        match self.worth.trim() {
            "" | "N/A" | "$0.00" => "FREE".to_owned(),
            w => format!("~~{w}~~ FREE"),
        }
    }

    /// `3 December 2026`, or `None` without an end date.
    pub fn until(&self) -> Option<String> {
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let date = self.end_date.split_whitespace().next()?;
        let mut parts = date.split('-').map(|p| p.parse::<usize>().ok());
        let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
        let month = MONTHS.get(month.checked_sub(1)?)?;
        Some(format!("{day} {month} {year}"))
    }
}

/// The store's free games (full games only, no DLC or loot) running right now.
pub async fn current(http: &reqwest::Client, shop: Shop) -> Result<Vec<Giveaway>, String> {
    let resp = http
        .get(API)
        .query(&[("platform", shop.api_platform()), ("type", "game")])
        .send()
        .await
        .map_err(|e| format!("GamerPower request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GamerPower returned HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("GamerPower response: {e}"))?;
    parse(body)
}

/// A list of giveaways; with none running, GamerPower answers with a status object instead.
fn parse(body: serde_json::Value) -> Result<Vec<Giveaway>, String> {
    if !body.is_array() {
        return Ok(Vec::new());
    }
    serde_json::from_value(body).map_err(|e| format!("GamerPower response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn giveaway(title: &str, worth: &str, end: &str) -> Giveaway {
        Giveaway {
            id: 1,
            title: title.into(),
            worth: worth.into(),
            image: String::new(),
            open_giveaway_url: "https://www.gamerpower.com/open/x".into(),
            end_date: end.into(),
        }
    }

    #[test]
    fn game_names() {
        let names = [
            ("Bread and Fred (Epic Games) Giveaway", "Bread and Fred"),
            ("Dead Age 2 (GOG) Giveaway", "Dead Age 2"),
            (
                "Vertical Drop Heroes HD Steam Key Giveaway",
                "Vertical Drop Heroes HD",
            ),
            ("Portal (Steam) Giveaway", "Portal"),
            ("Game (Remastered) Giveaway", "Game (Remastered)"),
        ];
        for (title, game) in names {
            assert_eq!(giveaway(title, "", "").game(), game);
        }
    }

    #[test]
    fn price_and_end() {
        let g = giveaway("x", "$14.99", "2026-12-03 23:59:00");
        assert_eq!(g.price(), "~~$14.99~~ FREE");
        assert_eq!(g.until().as_deref(), Some("3 December 2026"));
        let g = giveaway("x", "N/A", "N/A");
        assert_eq!(g.price(), "FREE");
        assert_eq!(g.until(), None);
    }

    #[test]
    fn shops() {
        assert_eq!(Shop::parse("Epic-Games"), Some(Shop::Epic));
        assert_eq!(Shop::parse("GOG"), Some(Shop::Gog));
        assert_eq!(Shop::parse("itch"), None);
        assert_eq!(Shop::from_key("steam"), Some(Shop::Steam));
    }

    #[test]
    fn parses_api_answers() {
        let none = serde_json::json!({ "status": 0, "status_message": "No active giveaways" });
        assert!(parse(none).unwrap().is_empty());
        let list = serde_json::json!([{
            "id": 3857, "title": "Bread and Fred (Epic Games) Giveaway", "worth": "$14.99",
            "image": "https://www.gamerpower.com/offers/1b/x.jpg",
            "open_giveaway_url": "https://www.gamerpower.com/open/bread-and-fred",
            "end_date": "2026-12-03 23:59:00", "users": 7910, "status": "Active"
        }]);
        let got = parse(list).unwrap();
        assert_eq!(got[0].id, 3857);
        assert_eq!(got[0].game(), "Bread and Fred");
    }

    /// Hits the real API: `cargo test -p botsu-core -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_api() {
        let http = reqwest::Client::new();
        for shop in Shop::ALL {
            let games = current(&http, shop).await.unwrap();
            println!(
                "{}: {:?}",
                shop.name(),
                games.iter().map(|g| g.game()).collect::<Vec<_>>()
            );
        }
    }
}
