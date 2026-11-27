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
use botsu_core::adapter::MAX_FLUXER_INSTANCES;
use botsu_core::alerts::AlertSettings;
use botsu_core::Platform;
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
    /// `BOTSU_CHANNEL=experimental`, set by the experimental Docker image.
    pub experimental: bool,
    pub owners: Vec<String>,
    pub data_dir: PathBuf,
    pub discord: Option<DiscordConfig>,
    /// Every Fluxer instance to connect to (self-hosted ones from `FLUXER_URL`, the official one
    /// from `FLUXER_OFFICIAL_TOKEN`).
    pub fluxer: Vec<FluxerConfig>,
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
        let prefix = if experimental {
            format!("{prefix}{prefix}")
        } else {
            prefix
        };

        let invite_admin = vars.flag("BOTSU_INVITE_ADMIN")?;
        let discord = vars.secret("DISCORD_TOKEN")?.map(|token| DiscordConfig {
            token,
            invite_admin,
        });

        let fluxer = fluxer_instances(vars, invite_admin)?;

        if discord.is_none() && fluxer.is_empty() {
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
            experimental,
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

/// A comma-separated list (blank entries dropped).
fn list(value: Option<String>) -> Vec<String> {
    value
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The Fluxer instances to connect to:
/// - `FLUXER_URL` (self-hosted addresses) and `FLUXER_TOKEN` (one token each, same order). The
///   first is `Platform::Fluxer`, so data from before several instances existed stays with it.
/// - `FLUXER_TOKEN` without `FLUXER_URL`: the official instance, as before (`Platform::Fluxer`).
/// - `FLUXER_OFFICIAL_TOKEN`: the official instance as an instance of its own.
/// - `FLUXER_API_URL`: skips discovery; only with a single instance from `FLUXER_TOKEN`.
fn fluxer_instances(vars: &Vars, invite_admin: bool) -> Result<Vec<FluxerConfig>> {
    let urls = list(vars.get("FLUXER_URL"));
    let tokens = list(vars.secret("FLUXER_TOKEN")?);
    let api = vars.get("FLUXER_API_URL");
    let mut out = Vec::new();

    if urls.is_empty() {
        match tokens.as_slice() {
            [] => {}
            [token] => out.push(FluxerConfig {
                platform: Platform::Fluxer,
                token: token.clone(),
                endpoint: api.map_or(FluxerEndpoint::Default, FluxerEndpoint::ApiUrl),
                invite_admin,
            }),
            _ => bail!(
                "FLUXER_TOKEN has {} tokens but FLUXER_URL has no addresses: give one address \
                 per token, in the same order",
                tokens.len()
            ),
        }
    } else {
        if urls.len() != tokens.len() {
            bail!(
                "FLUXER_URL has {} address(es) but FLUXER_TOKEN has {} token(s): give one token \
                 per address, in the same order",
                urls.len(),
                tokens.len()
            );
        }
        if api.is_some() && urls.len() > 1 {
            bail!("FLUXER_API_URL only works with a single FLUXER_URL");
        }
        for (i, (url, token)) in urls.iter().zip(&tokens).enumerate() {
            let Some(platform) = u8::try_from(i + 1).ok().and_then(Platform::fluxer_instance)
            else {
                bail!("FLUXER_URL lists more than {MAX_FLUXER_INSTANCES} instances");
            };
            let endpoint = match &api {
                Some(api) => FluxerEndpoint::ApiUrl(api.clone()),
                None => {
                    let Some(origin) = origin(url) else {
                        bail!("FLUXER_URL entries must start with https:// (got {url:?})");
                    };
                    if origin != url.trim_end_matches('/') {
                        // Common when copying the address bar from inside a channel.
                        tracing::warn!(
                            given = %url,
                            using = origin,
                            "FLUXER_URL should be just the site address; ignoring the path"
                        );
                    }
                    FluxerEndpoint::Instance(origin.to_owned())
                }
            };
            out.push(FluxerConfig {
                platform,
                token: token.clone(),
                endpoint,
                invite_admin,
            });
        }
    }

    if let Some(token) = vars.secret("FLUXER_OFFICIAL_TOKEN")? {
        if urls.is_empty() && !tokens.is_empty() {
            bail!(
                "FLUXER_TOKEN without FLUXER_URL already means the official Fluxer instance; \
                 use either that or FLUXER_OFFICIAL_TOKEN, not both"
            );
        }
        out.push(FluxerConfig {
            platform: Platform::FluxerOfficial,
            token,
            endpoint: FluxerEndpoint::Default,
            invite_admin,
        });
    }
    Ok(out)
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
    fn experimental_doubles_the_prefix() {
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
        assert_eq!((exp.prefix.as_str(), exp.experimental), ("!!", true));
    }

    #[test]
    fn fluxer_instances_from_lists() {
        // Only meaningful when the test's environment doesn't set Fluxer variables itself.
        let env_set = [
            "FLUXER_URL",
            "FLUXER_TOKEN",
            "FLUXER_OFFICIAL_TOKEN",
            "FLUXER_API_URL",
        ]
        .iter()
        .any(|k| std::env::var(k).is_ok());
        if env_set {
            return;
        }
        let vars = |pairs: &[(&str, &str)]| {
            Vars::new(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        };
        let platforms = |pairs: &[(&str, &str)]| -> Vec<Platform> {
            fluxer_instances(&vars(pairs), false)
                .unwrap()
                .iter()
                .map(|f| f.platform)
                .collect()
        };

        // One self-hosted instance: still Platform::Fluxer (existing data stays with it).
        assert_eq!(
            platforms(&[("FLUXER_URL", "https://a.example"), ("FLUXER_TOKEN", "t1")]),
            [Platform::Fluxer]
        );
        // A token without a URL: the official instance, as before.
        let legacy = fluxer_instances(&vars(&[("FLUXER_TOKEN", "t1")]), false).unwrap();
        assert!(matches!(legacy[0].endpoint, FluxerEndpoint::Default));
        assert_eq!(legacy[0].platform, Platform::Fluxer);
        // Two self-hosted plus the official one.
        let all = fluxer_instances(
            &vars(&[
                (
                    "FLUXER_URL",
                    "https://a.example, https://b.example/channels/1",
                ),
                ("FLUXER_TOKEN", "t1,t2"),
                ("FLUXER_OFFICIAL_TOKEN", "t3"),
            ]),
            false,
        )
        .unwrap();
        let got: Vec<(Platform, &str)> =
            all.iter().map(|f| (f.platform, f.token.as_str())).collect();
        assert_eq!(
            got,
            [
                (Platform::Fluxer, "t1"),
                (Platform::FluxerMore(2), "t2"),
                (Platform::FluxerOfficial, "t3")
            ]
        );
        assert!(
            matches!(&all[1].endpoint, FluxerEndpoint::Instance(o) if o == "https://b.example"),
            "path dropped"
        );
        // Mismatched lists, or the official instance twice: refused.
        assert!(fluxer_instances(
            &vars(&[
                ("FLUXER_URL", "https://a.example,https://b.example"),
                ("FLUXER_TOKEN", "t1")
            ]),
            false
        )
        .is_err());
        assert!(fluxer_instances(
            &vars(&[("FLUXER_TOKEN", "t1"), ("FLUXER_OFFICIAL_TOKEN", "t2")]),
            false
        )
        .is_err());
        assert!(fluxer_instances(&vars(&[]), false).unwrap().is_empty());
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
