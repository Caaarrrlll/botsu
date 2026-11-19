//! yt-dlp, run as a subprocess (phase 6).
//!
//! botsu manages its own copy in `<data dir>/bin` so it can be updated without a new image: it's
//! downloaded from yt-dlp's GitHub releases on first start (the standalone build, which needs no
//! Python and bundles the YouTube challenge scripts) and updated with `yt-dlp -U` once a day and
//! by `music update`. `BOTSU_YTDLP` points at another yt-dlp instead (a system install, for
//! example); botsu then leaves updating to whoever installed it.
//!
//! YouTube needs a JavaScript runtime for full support: the Docker image ships Deno.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error, Result};

/// How long one yt-dlp run may take.
const RUN_TIMEOUT: Duration = Duration::from_secs(90);
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);

/// Something found by a search or link: a single track, or one entry of a playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub title: String,
    /// The page URL (resolved to a stream URL only when it starts playing; those expire).
    pub url: String,
    pub duration: Option<u64>,
}

pub struct YtDlp {
    path: PathBuf,
    /// botsu downloaded it and keeps it updated.
    managed: bool,
    http: reqwest::Client,
    /// One download or update at a time.
    install: tokio::sync::Mutex<()>,
}

/// The release asset for this OS and CPU.
fn release_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("yt-dlp_linux"),
        ("linux", "aarch64") => Some("yt-dlp_linux_aarch64"),
        ("windows", _) => Some("yt-dlp.exe"),
        ("macos", _) => Some("yt-dlp_macos"),
        _ => None,
    }
}

impl YtDlp {
    /// `custom`: `BOTSU_YTDLP`, used as-is. Otherwise botsu's own copy in `<data dir>/bin`.
    pub fn new(data_dir: &Path, custom: Option<PathBuf>, http: reqwest::Client) -> Arc<Self> {
        let (path, managed) = match custom {
            Some(p) => (p, false),
            None => {
                let name = if cfg!(windows) {
                    "yt-dlp.exe"
                } else {
                    "yt-dlp"
                };
                (data_dir.join("bin").join(name), true)
            }
        };
        Arc::new(Self {
            path,
            managed,
            http,
            install: tokio::sync::Mutex::new(()),
        })
    }

    /// Downloads botsu's copy if it's missing.
    pub async fn ensure_installed(&self) -> Result<()> {
        if !self.managed || self.path.exists() {
            return Ok(());
        }
        let _guard = self.install.lock().await;
        if self.path.exists() {
            return Ok(());
        }
        let asset = release_asset().ok_or_else(|| {
            Error::Internal(format!(
                "no yt-dlp build for {}/{}; install it yourself and set BOTSU_YTDLP",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })?;
        let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}");
        tracing::info!(%url, path = %self.path.display(), "downloading yt-dlp");
        let bytes = self
            .http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| Error::internal("downloading yt-dlp", e))?
            .bytes()
            .await
            .map_err(|e| Error::internal("downloading yt-dlp", e))?;
        let dir = self.path.parent().unwrap_or(Path::new("."));
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| Error::internal("creating the yt-dlp folder", e))?;
        let tmp = self.path.with_extension("download");
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|e| Error::internal("saving yt-dlp", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .await
                .map_err(|e| Error::internal("making yt-dlp executable", e))?;
        }
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| Error::internal("saving yt-dlp", e))?;
        tracing::info!("yt-dlp downloaded");
        Ok(())
    }

    /// Updates botsu's copy (`yt-dlp -U`). Returns yt-dlp's last line of output.
    pub async fn update(&self) -> Result<String> {
        if !self.managed {
            return Ok(format!(
                "yt-dlp at {} is managed outside botsu (BOTSU_YTDLP); update it there.",
                self.path.display()
            ));
        }
        self.ensure_installed().await?;
        let _guard = self.install.lock().await;
        let out = self.run(&["-U"]).await?;
        Ok(out
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("done")
            .to_owned())
    }

    pub async fn version(&self) -> Option<String> {
        self.run(&["--version"])
            .await
            .ok()
            .map(|v| v.trim().to_owned())
    }

    /// A search (`ytsearch1:`) or a link: a video, or up to `limit` entries of a playlist.
    pub async fn lookup(&self, query: &str, limit: usize) -> Result<Vec<Found>> {
        let target = if is_url(query) {
            query.to_owned()
        } else {
            format!("ytsearch1:{query}")
        };
        let limit = limit.max(1).to_string();
        let out = self
            .run(&[
                "--flat-playlist",
                "--no-playlist",
                "-J",
                "--playlist-end",
                &limit,
                "--",
                &target,
            ])
            .await?;
        parse_lookup(&out)
    }

    /// A direct, short-lived audio stream URL for a page URL.
    pub async fn stream_url(&self, page_url: &str) -> Result<String> {
        let out = self
            .run(&[
                "-f",
                "bestaudio/best",
                "--no-playlist",
                "-g",
                "--",
                page_url,
            ])
            .await?;
        out.lines()
            .map(str::trim)
            .find(|l| l.starts_with("http"))
            .map(str::to_owned)
            .ok_or_else(|| Error::Internal("yt-dlp returned no stream URL".into()))
    }

    async fn run(&self, args: &[&str]) -> Result<String> {
        self.ensure_installed().await?;
        let child = tokio::process::Command::new(&self.path)
            .args(["--no-warnings", "--ignore-config"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Error::internal(&format!("running yt-dlp ({})", self.path.display()), e)
            })?;
        let out = tokio::time::timeout(RUN_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| Error::Internal("yt-dlp took too long".into()))?
            .map_err(|e| Error::internal("running yt-dlp", e))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let line = err
                .lines()
                .rev()
                .find(|l| l.contains("ERROR"))
                .or_else(|| err.lines().rev().find(|l| !l.trim().is_empty()))
                .unwrap_or("no output");
            return Err(Error::Internal(format!("yt-dlp failed: {}", line.trim())));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Installs yt-dlp at startup if needed, then updates it daily.
pub fn spawn_maintenance(ytdlp: Arc<YtDlp>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        match ytdlp.ensure_installed().await {
            Ok(()) => {
                let version = ytdlp.version().await;
                tracing::info!(?version, "yt-dlp ready")
            }
            Err(e) => tracing::warn!(error = %e, "yt-dlp isn't available; music won't play"),
        }
        loop {
            tokio::time::sleep(UPDATE_EVERY).await;
            match ytdlp.update().await {
                Ok(out) => tracing::info!(%out, "yt-dlp update check"),
                Err(e) => tracing::warn!(error = %e, "yt-dlp update failed"),
            }
        }
    })
}

pub fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// yt-dlp's `-J` output: one video, or a playlist (search results are a playlist too).
fn parse_lookup(json: &str) -> Result<Vec<Found>> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| Error::internal("reading yt-dlp output", e))?;
    let found = match v["entries"].as_array() {
        Some(entries) => entries.iter().filter_map(entry).collect(),
        None => entry(&v).into_iter().collect(),
    };
    Ok(found)
}

fn entry(v: &serde_json::Value) -> Option<Found> {
    let url = v["webpage_url"]
        .as_str()
        .filter(|u| is_url(u))
        .or_else(|| v["url"].as_str().filter(|u| is_url(u)))
        .map(str::to_owned)
        .or_else(|| {
            // Flat YouTube entries sometimes carry only the ID.
            let id = v["id"].as_str()?;
            (v["ie_key"].as_str() == Some("Youtube"))
                .then(|| format!("https://www.youtube.com/watch?v={id}"))
        })?;
    Some(Found {
        title: v["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or("Unknown title")
            .to_owned(),
        duration: v["duration"].as_f64().map(|d| d.round() as u64),
        url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_results_and_playlists() {
        let search = r#"{"_type":"playlist","entries":[
            {"ie_key":"Youtube","id":"abc","url":"https://www.youtube.com/watch?v=abc",
             "title":"Song","duration":213.0}]}"#;
        assert_eq!(
            parse_lookup(search).unwrap(),
            [Found {
                title: "Song".into(),
                url: "https://www.youtube.com/watch?v=abc".into(),
                duration: Some(213)
            }]
        );
        let playlist = r#"{"_type":"playlist","entries":[
            {"ie_key":"Youtube","id":"a","title":"One"},
            {"ie_key":"Youtube","id":"b","title":"","duration":null},
            {"title":"no url"}]}"#;
        let found = parse_lookup(playlist).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].url, "https://www.youtube.com/watch?v=b");
        assert_eq!(found[1].title, "Unknown title");
    }

    #[test]
    fn single_video() {
        let video =
            r#"{"id":"x","title":"Video","webpage_url":"https://youtu.be/x","duration":61.4}"#;
        let found = parse_lookup(video).unwrap();
        assert_eq!(found[0].url, "https://youtu.be/x");
        assert_eq!(found[0].duration, Some(61));
        assert!(parse_lookup("not json").is_err());
    }
}
