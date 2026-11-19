//! SQLite storage in `<data dir>/botsu.db`.
//!
//! Calls are synchronous behind a mutex. Every query here touches a handful of rows and takes
//! well under a millisecond, so blocking the async runtime briefly is fine; move to
//! `spawn_blocking` if anything heavier is ever added.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::adapter::Platform;

/// Schema changes, applied in order. Never edit a shipped entry; append a new one.
const MIGRATIONS: &[&str] = &[
    // 1: alerts
    "CREATE TABLE subscriptions (
        id          INTEGER PRIMARY KEY,
        kind        TEXT NOT NULL,      -- 'youtube' | 'twitch'
        source_id   TEXT NOT NULL,      -- YouTube channel ID (UC...) | Twitch user ID
        source_name TEXT NOT NULL,      -- display name when subscribed
        source_key  TEXT NOT NULL,      -- Twitch login (for URLs); YouTube: same as source_id
        platform    TEXT NOT NULL,      -- 'discord' | 'fluxer'
        guild_id    TEXT,
        channel_id  TEXT NOT NULL,
        created_by  TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        UNIQUE (kind, source_id, platform, channel_id)
    );
    CREATE TABLE seen_items (
        kind      TEXT NOT NULL,
        source_id TEXT NOT NULL,
        item_id   TEXT NOT NULL,        -- YouTube video ID | Twitch stream ID
        seen_at   INTEGER NOT NULL,
        PRIMARY KEY (kind, source_id, item_id)
    );
    CREATE TABLE source_state (
        kind         TEXT NOT NULL,
        source_id    TEXT NOT NULL,
        last_live_at INTEGER,           -- Twitch: last poll that saw the stream live
        PRIMARY KEY (kind, source_id)
    );",
    // 2: Shorts opt-in per subscription; scheduled YouTube streams/premieres waiting to go live
    "ALTER TABLE subscriptions ADD COLUMN include_shorts INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE pending_items (
        kind        TEXT NOT NULL,
        source_id   TEXT NOT NULL,
        source_name TEXT NOT NULL,
        item_id     TEXT NOT NULL,      -- YouTube video ID
        title       TEXT NOT NULL,
        url         TEXT NOT NULL,
        added_at    INTEGER NOT NULL,
        PRIMARY KEY (kind, item_id)
    );",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    YouTube,
    Twitch,
}

impl AlertKind {
    fn as_str(self) -> &'static str {
        match self {
            AlertKind::YouTube => "youtube",
            AlertKind::Twitch => "twitch",
        }
    }
}

fn platform_str(p: Platform) -> &'static str {
    match p {
        Platform::Discord => "discord",
        Platform::Fluxer => "fluxer",
    }
}

fn parse_platform(s: &str) -> Option<Platform> {
    match s {
        "discord" => Some(Platform::Discord),
        "fluxer" => Some(Platform::Fluxer),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: i64,
    pub kind: AlertKind,
    pub source_id: String,
    pub source_name: String,
    pub source_key: String,
    pub platform: Platform,
    pub guild_id: Option<String>,
    pub channel_id: String,
    /// YouTube: also post Shorts (off by default).
    pub include_shorts: bool,
}

/// A scheduled YouTube stream or premiere, checked every poll until it goes live.
#[derive(Debug, Clone)]
pub struct PendingItem {
    pub source_id: String,
    pub source_name: String,
    pub item_id: String,
    pub title: String,
    pub url: String,
    pub added_at: i64,
}

/// What a new subscription points at.
pub struct NewSubscription<'a> {
    pub kind: AlertKind,
    pub source_id: &'a str,
    pub source_name: &'a str,
    pub source_key: &'a str,
    pub platform: Platform,
    pub guild_id: Option<&'a str>,
    pub channel_id: &'a str,
    pub created_by: &'a str,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct Store {
    conn: Mutex<Connection>,
}

pub type DbResult<T> = rusqlite::Result<T>;

impl Store {
    /// Opens (creating if needed) `<dir>/botsu.db` and applies pending migrations.
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("can't create data directory {}: {e}", dir.display()))?;
        let path = dir.join("botsu.db");
        let conn = Connection::open(&path)
            .map_err(|e| format!("can't open database {}: {e}", path.display()))?;
        Self::init(conn).map_err(|e| format!("database {}: {e}", path.display()))
    }

    /// In-memory database, for tests.
    pub fn open_in_memory() -> DbResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> DbResult<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i as i64 + 1)?;
            tx.commit()?;
            tracing::info!(version = i + 1, "applied database migration");
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave SQLite inconsistent; keep going.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Adds a subscription. Returns false if the same source already posts to that channel.
    pub fn add_subscription(&self, s: &NewSubscription<'_>) -> DbResult<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO subscriptions
               (kind, source_id, source_name, source_key, platform, guild_id, channel_id,
                created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                s.kind.as_str(),
                s.source_id,
                s.source_name,
                s.source_key,
                platform_str(s.platform),
                s.guild_id,
                s.channel_id,
                s.created_by,
                now()
            ],
        )?;
        Ok(n > 0)
    }

    /// Removes subscriptions in one server (or DM channel) matching a source ID, key or name
    /// (case-insensitive). Returns how many were removed.
    pub fn remove_subscriptions(
        &self,
        kind: AlertKind,
        platform: Platform,
        scope: Scope<'_>,
        source: &str,
    ) -> DbResult<usize> {
        let (col, val) = scope.column();
        self.conn().execute(
            &format!(
                "DELETE FROM subscriptions
                 WHERE kind = ?1 AND platform = ?2 AND {col} = ?3
                   AND (source_id = ?4 OR lower(source_key) = lower(?4)
                        OR lower(source_name) = lower(?4))"
            ),
            params![kind.as_str(), platform_str(platform), val, source],
        )
    }

    pub fn list_subscriptions(
        &self,
        kind: AlertKind,
        platform: Platform,
        scope: Scope<'_>,
    ) -> DbResult<Vec<Subscription>> {
        let (col, val) = scope.column();
        self.query_subscriptions(
            &format!("WHERE kind = ?1 AND platform = ?2 AND {col} = ?3 ORDER BY source_name"),
            params![kind.as_str(), platform_str(platform), val],
        )
    }

    /// Every subscription of one kind, across all platforms (for the pollers).
    pub fn all_subscriptions(&self, kind: AlertKind) -> DbResult<Vec<Subscription>> {
        self.query_subscriptions("WHERE kind = ?1 ORDER BY source_id", params![kind.as_str()])
    }

    fn query_subscriptions(
        &self,
        filter: &str,
        args: impl rusqlite::Params,
    ) -> DbResult<Vec<Subscription>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, kind, source_id, source_name, source_key, platform, guild_id, channel_id,
                    include_shorts
             FROM subscriptions {filter}"
        ))?;
        let rows = stmt.query_map(args, |r| {
            let kind = match r.get::<_, String>(1)?.as_str() {
                "youtube" => Some(AlertKind::YouTube),
                "twitch" => Some(AlertKind::Twitch),
                _ => None,
            };
            // Rows for a kind or platform this build doesn't know are skipped, not fatal.
            let platform = parse_platform(&r.get::<_, String>(5)?);
            let (Some(kind), Some(platform)) = (kind, platform) else {
                return Ok(None);
            };
            Ok(Some(Subscription {
                id: r.get(0)?,
                kind,
                source_id: r.get(2)?,
                source_name: r.get(3)?,
                source_key: r.get(4)?,
                platform,
                guild_id: r.get(6)?,
                channel_id: r.get(7)?,
                include_shorts: r.get::<_, i64>(8)? != 0,
            }))
        })?;
        rows.filter_map(|r| r.transpose()).collect()
    }

    /// Turns Shorts on or off for matching subscriptions in one server. Returns how many changed.
    pub fn set_include_shorts(
        &self,
        platform: Platform,
        scope: Scope<'_>,
        source: &str,
        on: bool,
    ) -> DbResult<usize> {
        let (col, val) = scope.column();
        self.conn().execute(
            &format!(
                "UPDATE subscriptions SET include_shorts = ?1
                 WHERE kind = 'youtube' AND platform = ?2 AND {col} = ?3
                   AND (source_id = ?4 OR lower(source_key) = lower(?4)
                        OR lower(source_name) = lower(?4))"
            ),
            params![on as i64, platform_str(platform), val, source],
        )
    }

    pub fn add_pending(&self, kind: AlertKind, p: &PendingItem) -> DbResult<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO pending_items
               (kind, source_id, source_name, item_id, title, url, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                kind.as_str(),
                p.source_id,
                p.source_name,
                p.item_id,
                p.title,
                p.url,
                p.added_at
            ],
        )?;
        Ok(())
    }

    pub fn pending(&self, kind: AlertKind) -> DbResult<Vec<PendingItem>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT source_id, source_name, item_id, title, url, added_at
             FROM pending_items WHERE kind = ?1 ORDER BY added_at",
        )?;
        let rows = stmt.query_map(params![kind.as_str()], |r| {
            Ok(PendingItem {
                source_id: r.get(0)?,
                source_name: r.get(1)?,
                item_id: r.get(2)?,
                title: r.get(3)?,
                url: r.get(4)?,
                added_at: r.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn remove_pending(&self, kind: AlertKind, item_id: &str) -> DbResult<()> {
        self.conn().execute(
            "DELETE FROM pending_items WHERE kind = ?1 AND item_id = ?2",
            params![kind.as_str(), item_id],
        )?;
        Ok(())
    }

    /// True if anything has ever been recorded as seen for this source.
    pub fn has_seen_any(&self, kind: AlertKind, source_id: &str) -> DbResult<bool> {
        self.conn()
            .query_row(
                "SELECT 1 FROM seen_items WHERE kind = ?1 AND source_id = ?2 LIMIT 1",
                params![kind.as_str(), source_id],
                |_| Ok(()),
            )
            .optional()
            .map(|r| r.is_some())
    }

    pub fn is_seen(&self, kind: AlertKind, source_id: &str, item_id: &str) -> DbResult<bool> {
        self.conn()
            .query_row(
                "SELECT 1 FROM seen_items WHERE kind = ?1 AND source_id = ?2 AND item_id = ?3",
                params![kind.as_str(), source_id, item_id],
                |_| Ok(()),
            )
            .optional()
            .map(|r| r.is_some())
    }

    pub fn mark_seen(&self, kind: AlertKind, source_id: &str, item_id: &str) -> DbResult<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO seen_items (kind, source_id, item_id, seen_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![kind.as_str(), source_id, item_id, now()],
        )?;
        Ok(())
    }

    /// Drops seen items older than `max_age_secs`, keeping the table small.
    /// Old items can't come back: the YouTube feed only lists the latest ~15 videos.
    pub fn prune_seen(&self, max_age_secs: i64) -> DbResult<usize> {
        self.conn().execute(
            "DELETE FROM seen_items WHERE seen_at < ?1",
            params![now() - max_age_secs],
        )
    }

    pub fn last_live_at(&self, kind: AlertKind, source_id: &str) -> DbResult<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT last_live_at FROM source_state WHERE kind = ?1 AND source_id = ?2",
                params![kind.as_str(), source_id],
                |r| r.get(0),
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn set_last_live_at(&self, kind: AlertKind, source_id: &str, at: i64) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO source_state (kind, source_id, last_live_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (kind, source_id) DO UPDATE SET last_live_at = excluded.last_live_at",
            params![kind.as_str(), source_id, at],
        )?;
        Ok(())
    }
}

/// Which subscriptions a command can see: those of the current server, or of the current
/// channel when there's no server (DMs).
#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    Guild(&'a str),
    Channel(&'a str),
}

impl<'a> Scope<'a> {
    pub fn of(guild_id: Option<&'a str>, channel_id: &'a str) -> Self {
        match guild_id {
            Some(g) => Scope::Guild(g),
            None => Scope::Channel(channel_id),
        }
    }

    fn column(self) -> (&'static str, &'a str) {
        match self {
            Scope::Guild(g) => ("guild_id", g),
            Scope::Channel(c) => ("channel_id", c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub<'a>(source: &'a str, channel: &'a str) -> NewSubscription<'a> {
        NewSubscription {
            kind: AlertKind::YouTube,
            source_id: source,
            source_name: "Some Channel",
            source_key: source,
            platform: Platform::Discord,
            guild_id: Some("g1"),
            channel_id: channel,
            created_by: "u1",
        }
    }

    #[test]
    fn subscriptions_are_unique_per_channel_and_removable_by_name() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.add_subscription(&sub("UC1", "c1")).unwrap());
        assert!(!store.add_subscription(&sub("UC1", "c1")).unwrap());
        assert!(store.add_subscription(&sub("UC1", "c2")).unwrap());

        let scope = Scope::Guild("g1");
        let listed = store
            .list_subscriptions(AlertKind::YouTube, Platform::Discord, scope)
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(store
            .list_subscriptions(AlertKind::YouTube, Platform::Fluxer, scope)
            .unwrap()
            .is_empty());

        let removed = store
            .remove_subscriptions(AlertKind::YouTube, Platform::Discord, scope, "some channel")
            .unwrap();
        assert_eq!(removed, 2);
    }

    #[test]
    fn shorts_toggle_and_pending_items() {
        let store = Store::open_in_memory().unwrap();
        store.add_subscription(&sub("UC1", "c1")).unwrap();
        let scope = Scope::Guild("g1");
        let get = || store.all_subscriptions(AlertKind::YouTube).unwrap()[0].include_shorts;
        assert!(!get(), "Shorts are off by default");
        assert_eq!(
            store
                .set_include_shorts(Platform::Discord, scope, "some channel", true)
                .unwrap(),
            1
        );
        assert!(get());
        assert_eq!(
            store
                .set_include_shorts(Platform::Fluxer, scope, "UC1", false)
                .unwrap(),
            0
        );
        let p = PendingItem {
            source_id: "UC1".into(),
            source_name: "Some Channel".into(),
            item_id: "vid".into(),
            title: "Stream".into(),
            url: "https://www.youtube.com/watch?v=vid".into(),
            added_at: 1,
        };
        store.add_pending(AlertKind::YouTube, &p).unwrap();
        store.add_pending(AlertKind::YouTube, &p).unwrap();
        assert_eq!(store.pending(AlertKind::YouTube).unwrap().len(), 1);
        store.remove_pending(AlertKind::YouTube, "vid").unwrap();
        assert!(store.pending(AlertKind::YouTube).unwrap().is_empty());
    }
    #[test]
    fn upgrades_a_version_1_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute("INSERT INTO subscriptions (kind, source_id, source_name, source_key, platform, channel_id, created_by, created_at) VALUES ('youtube','UC1','n','UC1','discord','c','u',0)", []).unwrap();
        let store = Store::init(conn).unwrap();
        let subs = store.all_subscriptions(AlertKind::YouTube).unwrap();
        assert_eq!(subs.len(), 1);
        assert!(!subs[0].include_shorts);
    }
    #[test]
    fn seen_items_and_live_state() {
        let store = Store::open_in_memory().unwrap();
        assert!(!store.has_seen_any(AlertKind::YouTube, "UC1").unwrap());
        store.mark_seen(AlertKind::YouTube, "UC1", "vid").unwrap();
        store.mark_seen(AlertKind::YouTube, "UC1", "vid").unwrap();
        assert!(store.has_seen_any(AlertKind::YouTube, "UC1").unwrap());
        assert!(store.is_seen(AlertKind::YouTube, "UC1", "vid").unwrap());
        assert!(!store.is_seen(AlertKind::Twitch, "UC1", "vid").unwrap());

        assert_eq!(store.last_live_at(AlertKind::Twitch, "42").unwrap(), None);
        store
            .set_last_live_at(AlertKind::Twitch, "42", 100)
            .unwrap();
        store
            .set_last_live_at(AlertKind::Twitch, "42", 200)
            .unwrap();
        assert_eq!(
            store.last_live_at(AlertKind::Twitch, "42").unwrap(),
            Some(200)
        );
    }
}
