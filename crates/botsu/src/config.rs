//! Configuration from environment variables only (see `.env.example`).
//! On Unraid these come from the root-only env file passed with `--env-file`.
//! A platform is enabled by setting its token; at least one is required.

use anyhow::{bail, Result};
use botsu_discord::DiscordConfig;
use botsu_fluxer::FluxerConfig;

/// Red's prefix, so botsu is a drop-in replacement after cutover.
/// Override with `BOTSU_PREFIX` (for example `!` while Red is still running).
pub const DEFAULT_PREFIX: &str = "$";

pub struct Config {
    pub prefix: String,
    pub discord: Option<DiscordConfig>,
    pub fluxer: Option<FluxerConfig>,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
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

        let fluxer = secret("FLUXER_TOKEN")?.map(|token| FluxerConfig {
            token,
            api_url: var("FLUXER_API_URL")
                .unwrap_or_else(|| botsu_fluxer::DEFAULT_API_URL.to_owned()),
        });

        if discord.is_none() && fluxer.is_none() {
            bail!("no platform configured: set DISCORD_TOKEN and/or FLUXER_TOKEN");
        }

        Ok(Self {
            prefix,
            discord,
            fluxer,
        })
    }
}
