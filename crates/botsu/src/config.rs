//! Configuration from environment variables only (see `.env.example`).
//! On Unraid these come from the root-only env file passed with `--env-file`.
//! A platform is enabled by setting its token; at least one is required.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use botsu_core::alerts::AlertSettings;
use botsu_discord::DiscordConfig;
use botsu_fluxer::{FluxerConfig, FluxerEndpoint};

/// Red's prefix, so botsu is a drop-in replacement after cutover.
/// Override with `BOTSU_PREFIX` (for example `!` while Red is still running).
pub const DEFAULT_PREFIX: &str = "$";

pub struct Config {
    pub prefix: String,
    pub owners: Vec<String>,
    pub data_dir: PathBuf,
    pub discord: Option<DiscordConfig>,
    pub fluxer: Option<FluxerConfig>,
    pub youtube_api_key: Option<String>,
    /// (client ID, client secret)
    pub twitch: Option<(String, String)>,
    pub alerts: AlertSettings,
}

/// Reads a poll interval in seconds, with a floor so nobody hammers YouTube or Twitch.
fn interval(name: &str, default: u64, min: u64) -> Result<Duration> {
    let secs = match var(name) {
        None => default,
        Some(v) => v
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("{name} must be a number of seconds (got {v:?})"))?,
    };
    if secs < min {
        bail!("{name} must be at least {min} seconds (got {secs})");
    }
    Ok(Duration::from_secs(secs))
}

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// `https://host[:port]/any/path?q` → `https://host[:port]`. None if it isn't an http(s) URL.
fn origin(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if host_len == 0 {
        return None;
    }
    Some(&url[..url.len() - rest.len() + host_len])
}

/// Reads a secret, rejecting values that still carry quotes:
/// Docker env files keep quotes as part of the value.
fn secret(name: &str) -> Result<Option<String>> {
    match var(name) {
        Some(v) if v.starts_with(['"', '\'']) => {
            bail!("{name} starts with a quote; remove quotes from the env file")
        }
        other => Ok(other),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let prefix = var("BOTSU_PREFIX").unwrap_or_else(|| DEFAULT_PREFIX.to_owned());
        if prefix.chars().any(char::is_whitespace) {
            bail!("BOTSU_PREFIX must not contain spaces (got {prefix:?})");
        }

        let discord = secret("DISCORD_TOKEN")?.map(|token| DiscordConfig { token });

        // FLUXER_API_URL (explicit) wins over FLUXER_URL (discovered); neither means fluxer.app.
        let endpoint = match (var("FLUXER_API_URL"), var("FLUXER_URL")) {
            (Some(api), _) => FluxerEndpoint::ApiUrl(api),
            (None, Some(instance)) => {
                let Some(origin) = origin(&instance) else {
                    bail!("FLUXER_URL must start with https:// (got {instance:?})");
                };
                if origin != instance.trim_end_matches('/') {
                    // Common when copying the address bar from inside a channel.
                    tracing::warn!(
                        given = %instance,
                        using = origin,
                        "FLUXER_URL should be just the site address; ignoring the path"
                    );
                }
                FluxerEndpoint::Instance(origin.to_owned())
            }
            (None, None) => FluxerEndpoint::Default,
        };
        let fluxer = secret("FLUXER_TOKEN")?.map(|token| FluxerConfig { token, endpoint });

        if discord.is_none() && fluxer.is_none() {
            bail!("no platform configured: set DISCORD_TOKEN and/or FLUXER_TOKEN");
        }

        let twitch = match (secret("TWITCH_CLIENT_ID")?, secret("TWITCH_CLIENT_SECRET")?) {
            (Some(id), Some(secret)) => Some((id, secret)),
            (None, None) => None,
            _ => bail!("set both TWITCH_CLIENT_ID and TWITCH_CLIENT_SECRET, or neither"),
        };

        Ok(Self {
            prefix,
            owners: var("BOTSU_OWNERS")
                .map(|v| parse_owners(&v))
                .unwrap_or_default(),
            // Relative paths are relative to where the bot starts (the repo when run locally).
            // The Docker image sets /data.
            data_dir: var("BOTSU_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data")),
            discord,
            fluxer,
            youtube_api_key: secret("YOUTUBE_API_KEY")?,
            twitch,
            alerts: AlertSettings {
                youtube_interval: interval("YOUTUBE_POLL_SECONDS", 300, 60)?,
                twitch_interval: interval("TWITCH_POLL_SECONDS", 120, 30)?,
            },
        })
    }
}

/// `"123, 456 789"` → `["123", "456", "789"]`. IDs from any platform can be mixed.
fn parse_owners(v: &str) -> Vec<String> {
    v.split([',', ' ', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{origin, parse_owners};

    #[test]
    fn owners_list() {
        assert_eq!(parse_owners("123, 456 789;"), vec!["123", "456", "789"]);
        assert!(parse_owners(" , ").is_empty());
    }

    #[test]
    fn origin_strips_paths() {
        assert_eq!(
            origin("https://fluxer.example.org/channels/123/456"),
            Some("https://fluxer.example.org")
        );
        assert_eq!(
            origin("https://fluxer.example.org/"),
            Some("https://fluxer.example.org")
        );
        assert_eq!(
            origin("http://192.0.2.10:8080?x=1"),
            Some("http://192.0.2.10:8080")
        );
        assert_eq!(origin("fluxer.example.org"), None);
        assert_eq!(origin("https:///nohost"), None);
    }
}
