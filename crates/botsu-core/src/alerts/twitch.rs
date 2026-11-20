//! Twitch live alerts via the Helix API with an app access token (client-credentials flow).
//! Needs `TWITCH_CLIENT_ID` and `TWITCH_CLIENT_SECRET` from https://dev.twitch.tv/console.

use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const HELIX: &str = "https://api.twitch.tv/helix";

pub struct Twitch {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    /// Cached app token and when to stop using it.
    token: Mutex<Option<(String, Instant)>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub login: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Stream {
    /// Changes with every new broadcast; used to post each stream once.
    pub id: String,
    pub user_id: String,
    pub user_login: String,
    pub user_name: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub title: String,
    /// Live frame, with `{width}` and `{height}` placeholders.
    #[serde(default)]
    pub thumbnail_url: String,
}

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

impl Twitch {
    pub fn new(http: reqwest::Client, client_id: String, client_secret: String) -> Self {
        Self {
            http,
            client_id,
            client_secret,
            token: Mutex::new(None),
        }
    }

    async fn token(&self) -> Result<String, String> {
        let mut cached = self.token.lock().await;
        if let Some((token, until)) = cached.as_ref() {
            if Instant::now() < *until {
                return Ok(token.clone());
            }
        }
        let resp = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(|e| format!("Twitch token request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(format!(
                "Twitch rejected TWITCH_CLIENT_ID / TWITCH_CLIENT_SECRET (HTTP {status})"
            ));
        }
        let t: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Twitch token response: {e}"))?;
        // Renew a minute early so a request never goes out with an expiring token.
        let until = Instant::now() + Duration::from_secs(t.expires_in.saturating_sub(60));
        *cached = Some((t.access_token.clone(), until));
        tracing::debug!(valid_for_s = t.expires_in, "got Twitch app token");
        Ok(t.access_token)
    }

    /// GET a Helix endpoint. On 401 (token revoked or expired early) fetches a new token and
    /// retries once.
    async fn get<T: DeserializeOwned>(&self, path_and_query: &str) -> Result<T, String> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let resp = self
                .http
                .get(format!("{HELIX}{path_and_query}"))
                .header("Client-Id", &self.client_id)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| format!("Twitch request failed: {e}"))?;
            match resp.status().as_u16() {
                401 if attempt == 0 => {
                    *self.token.lock().await = None;
                    continue;
                }
                200 => {
                    return resp
                        .json()
                        .await
                        .map_err(|e| format!("Twitch response: {e}"))
                }
                s => return Err(format!("Twitch returned HTTP {s}")),
            }
        }
        Err("Twitch kept rejecting the app token".into())
    }

    /// Checks the credentials work; used at startup for a clear error.
    pub async fn check(&self) -> Result<(), String> {
        self.token().await.map(|_| ())
    }

    pub async fn user_by_login(&self, login: &str) -> Result<Option<User>, String> {
        let page: Page<User> = self.get(&format!("/users?login={login}")).await?;
        Ok(page.data.into_iter().next())
    }

    /// Looks up many logins at once (100 per request). Unknown logins are simply absent.
    pub async fn users_by_login(&self, logins: &[String]) -> Result<Vec<User>, String> {
        let mut users = Vec::new();
        for chunk in logins.chunks(100) {
            let query: Vec<String> = chunk.iter().map(|l| format!("login={l}")).collect();
            let page: Page<User> = self.get(&format!("/users?{}", query.join("&"))).await?;
            users.extend(page.data);
        }
        Ok(users)
    }

    /// Live streams among `user_ids` (offline users are simply absent).
    pub async fn live_streams(&self, user_ids: &[String]) -> Result<Vec<Stream>, String> {
        let mut live = Vec::new();
        for chunk in user_ids.chunks(100) {
            let query: Vec<String> = chunk.iter().map(|id| format!("user_id={id}")).collect();
            let page: Page<Stream> = self
                .get(&format!("/streams?first=100&{}", query.join("&")))
                .await?;
            live.extend(page.data);
        }
        Ok(live)
    }
}

/// `https://twitch.tv/Name`, `twitch.tv/name`, `<url>` or `name` → `name` (lower case).
pub fn parse_login(input: &str) -> Option<String> {
    let s = input.trim().trim_start_matches('<').trim_end_matches('>');
    let s = s
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let s = s.strip_prefix("twitch.tv/").unwrap_or(s);
    let login = s.split(['/', '?', '#']).next()?.to_lowercase();
    let valid = (1..=25).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    valid.then_some(login)
}

#[cfg(test)]
mod tests {
    use super::parse_login;

    #[test]
    fn parses_logins() {
        assert_eq!(parse_login("SomeStreamer").as_deref(), Some("somestreamer"));
        assert_eq!(
            parse_login("<https://www.twitch.tv/Some_Streamer/videos>").as_deref(),
            Some("some_streamer")
        );
        assert_eq!(parse_login("twitch.tv/abc?x=1").as_deref(), Some("abc"));
        assert_eq!(parse_login("not a name"), None);
        assert_eq!(parse_login(""), None);
    }
}
