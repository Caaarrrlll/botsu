//! Configuration: environment variables first, then the settings file `<data dir>/botsu.env`.
//!
//! - Locally the environment comes from `.env` (scripts/run.ps1, VS Code).
//! - In Docker, settings can come from `-e`/`--env-file`, or from `/data/botsu.env`, which the bot
//!   creates on first start (so an Unraid user only fills in one file in appdata).
//!
//! A platform is enabled by setting its token; at least one is required.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use botsu_core::alerts::AlertSettings;
use botsu_discord::DiscordConfig;
use botsu_fluxer::{FluxerConfig, FluxerEndpoint};

/// Red's prefix, so botsu is a drop-in replacement after cutover.
/// Override with `BOTSU_PREFIX` (for example `!` while Red is still running).
pub const DEFAULT_PREFIX: &str = "$";

/// Name of the settings file inside the data directory.
pub const SETTINGS_FILE: &str = "botsu.env";

/// What a new settings file starts as: the documented example, plus a header.
const SETTINGS_TEMPLATE: &str = concat!(
    "# botsu settings. Created automatically on first start; fill in at least one token.\n",
    "# The bot checks this file every 30 seconds until a platform is configured, then starts.\n",
    "# After that, restart the container to apply changes.\n",
    "# Environment variables (docker -e / --env-file) override anything set here.\n",
    "#\n",
    include_str!("../../../.env.example")
);

pub struct Config {
    /// The command prefix in use: `BOTSU_PREFIX` (or `$`), doubled on the experimental channel.
    pub prefix: String,
    /// `BOTSU_PREFIX` (or `$`) before doubling.
    pub base_prefix: String,
    /// `BOTSU_CHANNEL=experimental`, set by the experimental Docker image.
    pub experimental: bool,
    /// `BOTSU_OTHER_INVITES`: the other channel's bot's invite links, listed by `invite`.
    pub other_invites: Vec<String>,
    pub owners: Vec<String>,
    pub data_dir: PathBuf,
    pub discord: Option<DiscordConfig>,
    pub fluxer: Option<FluxerConfig>,
    pub youtube_api_key: Option<String>,
    /// (client ID, client secret)
    pub twitch: Option<(String, String)>,
    pub alerts: AlertSettings,
    /// `BOTSU_YTDLP`: a yt-dlp to use instead of botsu's own copy in `<data dir>/bin`.
    pub ytdlp: Option<PathBuf>,
}

/// Where values come from: the process environment, then the settings file.
pub struct Vars {
    file: HashMap<String, String>,
}

impl Vars {
    pub fn new(file: HashMap<String, String>) -> Self {
        Self { file }
    }

    /// Empty values count as unset, so an empty `-e KEY=` doesn't hide the file's value.
    fn get(&self, name: &str) -> Option<String> {
        let clean = |v: &str| Some(v.trim().to_owned()).filter(|v| !v.is_empty());
        std::env::var(name)
            .ok()
            .and_then(|v| clean(&v))
            .or_else(|| self.file.get(name).and_then(|v| clean(v)))
    }

    /// Reads a secret, rejecting values that still carry quotes:
    /// Docker env files keep quotes as part of the value.
    fn secret(&self, name: &str) -> Result<Option<String>> {
        match self.get(name) {
            Some(v) if v.starts_with(['"', '\'']) => {
                bail!("{name} starts with a quote; remove quotes from the settings/env file")
            }
            other => Ok(other),
        }
    }

    /// A poll interval in seconds, with a floor so nobody hammers YouTube or Twitch.
    /// An on/off setting: `true`/`yes`/`on`/`1` or `false`/`no`/`off`/`0`; unset = `false`.
    fn flag(&self, name: &str) -> Result<bool> {
        match self.get(name).map(|v| v.to_lowercase()).as_deref() {
            None | Some("false" | "no" | "off" | "0") => Ok(false),
            Some("true" | "yes" | "on" | "1") => Ok(true),
            Some(other) => bail!("{name} must be true or false (got {other:?})"),
        }
    }

    fn interval(&self, name: &str, default: u64, min: u64) -> Result<Duration> {
        let secs = match self.get(name) {
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
}

/// The data directory comes from the environment only (it's where the settings file lives).
/// Relative paths are relative to where the bot starts; the Docker image sets /data.
pub fn data_dir() -> PathBuf {
    std::env::var("BOTSU_DATA_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"))
}

/// Parses a settings file with Docker `--env-file` rules: `KEY=value` per line, `#` comments,
/// no quote handling. A missing file is an empty map.
pub fn read_settings(path: &Path) -> Result<HashMap<String, String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("can't read {}", path.display())),
    };
    Ok(parse_settings(&text))
}

fn parse_settings(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .filter(|(k, _)| !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .collect()
}

/// Writes the commented settings template, readable and writable by owner and group only
/// (Unraid's `users` group can edit it over the appdata share; others can't read the tokens).
pub fn write_settings_template(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    std::fs::write(path, SETTINGS_TEMPLATE)
        .with_context(|| format!("can't write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
            .with_context(|| format!("can't set permissions on {}", path.display()))?;
    }
    Ok(())
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

impl Config {
    /// `Ok(None)` when no platform token is set yet (the caller waits for one); `Err` for
    /// settings that are present but wrong.
    pub fn load(vars: &Vars, data_dir: PathBuf) -> Result<Option<Self>> {
        let prefix = vars
            .get("BOTSU_PREFIX")
            .unwrap_or_else(|| DEFAULT_PREFIX.to_owned());
        if prefix.chars().any(char::is_whitespace) {
            bail!("BOTSU_PREFIX must not contain spaces (got {prefix:?})");
        }
        // The experimental bot doubles it ($ → $$), so it can share servers with the stable one.
        let experimental = match vars
            .get("BOTSU_CHANNEL")
            .map(|v| v.to_lowercase())
            .as_deref()
        {
            None | Some("stable") => false,
            Some("experimental") => true,
            Some(other) => bail!("BOTSU_CHANNEL must be stable or experimental (got {other:?})"),
        };
        let base_prefix = prefix.clone();
        let prefix = if experimental {
            format!("{prefix}{prefix}")
        } else {
            prefix
        };
        let other_invites: Vec<String> = vars
            .get("BOTSU_OTHER_INVITES")
            .map(|v| {
                v.split([',', ' ', ';'])
                    .map(str::trim)
                    .filter(|s| s.starts_with("https://"))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let invite_admin = vars.flag("BOTSU_INVITE_ADMIN")?;
        let discord = vars.secret("DISCORD_TOKEN")?.map(|token| DiscordConfig {
            token,
            invite_admin,
        });

        // FLUXER_API_URL (explicit) wins over FLUXER_URL (discovered); neither means fluxer.app.
        let endpoint = match (vars.get("FLUXER_API_URL"), vars.get("FLUXER_URL")) {
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
        let fluxer = vars.secret("FLUXER_TOKEN")?.map(|token| FluxerConfig {
            token,
            endpoint,
            invite_admin,
        });

        if discord.is_none() && fluxer.is_none() {
            return Ok(None);
        }

        let twitch = match (
            vars.secret("TWITCH_CLIENT_ID")?,
            vars.secret("TWITCH_CLIENT_SECRET")?,
        ) {
            (Some(id), Some(secret)) => Some((id, secret)),
            (None, None) => None,
            _ => bail!("set both TWITCH_CLIENT_ID and TWITCH_CLIENT_SECRET, or neither"),
        };

        Ok(Some(Self {
            prefix,
            base_prefix,
            experimental,
            other_invites,
            owners: vars
                .get("BOTSU_OWNERS")
                .map(|v| parse_owners(&v))
                .unwrap_or_default(),
            data_dir,
            discord,
            fluxer,
            youtube_api_key: vars.secret("YOUTUBE_API_KEY")?,
            twitch,
            alerts: AlertSettings {
                youtube_interval: vars.interval("YOUTUBE_POLL_SECONDS", 300, 60)?,
                twitch_interval: vars.interval("TWITCH_POLL_SECONDS", 120, 30)?,
            },
            ytdlp: vars.get("BOTSU_YTDLP").map(PathBuf::from),
        }))
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
    use super::*;

    #[test]
    fn on_off_flags() {
        let vars = |v: &str| {
            let mut m = std::collections::HashMap::new();
            m.insert("BOTSU_TEST_FLAG".to_owned(), v.to_owned());
            Vars::new(m)
        };
        assert!(vars("true").flag("BOTSU_TEST_FLAG").unwrap());
        assert!(vars("On").flag("BOTSU_TEST_FLAG").unwrap());
        assert!(!vars("0").flag("BOTSU_TEST_FLAG").unwrap());
        assert!(!vars("").flag("BOTSU_TEST_FLAG").unwrap());
        assert!(vars("maybe").flag("BOTSU_TEST_FLAG").is_err());
    }

    #[test]
    fn experimental_doubles_the_prefix_and_reads_other_invites() {
        let load = |pairs: &[(&str, &str)]| {
            let mut m: HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            m.insert("DISCORD_TOKEN".into(), "x".into());
            Config::load(&Vars::new(m), PathBuf::from("data"))
                .unwrap()
                .unwrap()
        };
        // Only meaningful when the test's environment doesn't set these.
        if std::env::var("BOTSU_CHANNEL").is_ok() || std::env::var("BOTSU_PREFIX").is_ok() {
            return;
        }
        let stable = load(&[]);
        assert_eq!((stable.prefix.as_str(), stable.experimental), ("$", false));
        let exp = load(&[("BOTSU_CHANNEL", "experimental"), ("BOTSU_PREFIX", "!")]);
        assert_eq!((exp.prefix.as_str(), exp.base_prefix.as_str()), ("!!", "!"));
        let other = load(&[(
            "BOTSU_OTHER_INVITES",
            "https://discord.com/oauth2/authorize?client_id=1, junk https://f.example/o?c=2",
        )]);
        assert_eq!(other.other_invites.len(), 2);
    }

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

    #[test]
    fn settings_file_rules() {
        let map = parse_settings(
            "# comment\nBOTSU_PREFIX=!\n  FLUXER_URL = https://x.example \nEMPTY=\nnot a line\nBAD KEY=1\n",
        );
        assert_eq!(map.get("BOTSU_PREFIX").map(String::as_str), Some("!"));
        assert_eq!(
            map.get("FLUXER_URL").map(String::as_str),
            Some("https://x.example")
        );
        assert_eq!(map.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn template_has_no_token_and_loads_as_unconfigured() {
        let map = parse_settings(SETTINGS_TEMPLATE);
        assert!(map.get("DISCORD_TOKEN").is_some_and(|v| v.is_empty()));
        // Only meaningful when the test's own environment has no tokens set.
        if std::env::var("DISCORD_TOKEN").is_err() && std::env::var("FLUXER_TOKEN").is_err() {
            let cfg = Config::load(&Vars::new(map), PathBuf::from("data")).unwrap();
            assert!(cfg.is_none());
        }
    }
}
