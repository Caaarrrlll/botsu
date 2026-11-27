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
    // 3: per-server admin and mod roles (permissions)
    "CREATE TABLE guild_roles (
        platform TEXT NOT NULL,
        guild_id TEXT NOT NULL,
        level    TEXT NOT NULL,         -- 'admin' | 'mod'
        role_id  TEXT NOT NULL,
        PRIMARY KEY (platform, guild_id, level, role_id)
    );",
    // 4: moderation cases, temp-ban expiry, per-server settings (mod-log channel)
    "CREATE TABLE mod_cases (
        platform     TEXT NOT NULL,
        guild_id     TEXT NOT NULL,
        number       INTEGER NOT NULL,  -- case number, counted per server
        action       TEXT NOT NULL,     -- 'warn' | 'kick' | 'ban' | 'tempban' | 'unban' | 'timeout' | 'untimeout'
        target_id    TEXT NOT NULL,
        moderator_id TEXT NOT NULL,     -- '' = botsu itself (temp-ban ended)
        reason       TEXT,
        created_at   INTEGER NOT NULL,
        expires_at   INTEGER,           -- tempban / timeout end
        revoked_at   INTEGER,           -- warnings removed with unwarn
        PRIMARY KEY (platform, guild_id, number)
    );
    CREATE INDEX mod_cases_target ON mod_cases (platform, guild_id, target_id);
    CREATE TABLE scheduled_unbans (
        platform    TEXT NOT NULL,
        guild_id    TEXT NOT NULL,
        user_id     TEXT NOT NULL,
        unban_at    INTEGER NOT NULL,
        case_number INTEGER NOT NULL,   -- the tempban case
        attempts    INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (platform, guild_id, user_id)
    );
    CREATE TABLE guild_settings (
        platform TEXT NOT NULL,
        guild_id TEXT NOT NULL,
        key      TEXT NOT NULL,         -- 'modlog_channel'
        value    TEXT NOT NULL,
        PRIMARY KEY (platform, guild_id, key)
    );",
    // 5: custom alert text and a role (or @everyone/@here) to mention, per subscription
    "ALTER TABLE subscriptions ADD COLUMN message TEXT;  -- NULL = the default text
    ALTER TABLE subscriptions ADD COLUMN mention TEXT;  -- role ID | 'user:<ID>' | 'everyone' | 'here' | NULL",
    // 6: mutes with the server's mute role that end by themselves
    "CREATE TABLE scheduled_unmutes (
        platform    TEXT NOT NULL,
        guild_id    TEXT NOT NULL,
        user_id     TEXT NOT NULL,
        role_id     TEXT NOT NULL,      -- the mute role given (removed when due)
        unmute_at   INTEGER NOT NULL,
        case_number INTEGER NOT NULL,   -- the mute case
        attempts    INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (platform, guild_id, user_id)
    );",
    // 7: custom commands (`cc`)
    "CREATE TABLE custom_commands (
        platform   TEXT NOT NULL,
        guild_id   TEXT NOT NULL,
        name       TEXT NOT NULL,      -- lower case, without the prefix
        response   TEXT NOT NULL,
        created_by TEXT NOT NULL,      -- user ID of the last create/edit
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (platform, guild_id, name)
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
    p.key()
}

fn parse_platform(s: &str) -> Option<Platform> {
    Platform::from_key(s)
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
    /// User ID that added it (may remove it and toggle its Shorts).
    pub created_by: String,
    /// Custom alert text with `{name}`, `{title}`, `{url}`, `{game}`, `{event}`; `None` = default.
    pub message: Option<String>,
    /// Role ID, `user:<ID>`, `everyone` or `here`, mentioned at the start of each alert.
    pub mention: Option<String>,
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

/// A moderation case.
#[derive(Debug, Clone)]
pub struct Case {
    pub number: i64,
    pub action: String,
    pub target_id: String,
    /// Empty when botsu did it by itself (a temp-ban ending).
    pub moderator_id: String,
    pub reason: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

pub struct NewCase<'a> {
    pub platform: Platform,
    pub guild_id: &'a str,
    pub action: &'a str,
    pub target_id: &'a str,
    pub moderator_id: &'a str,
    pub reason: Option<&'a str>,
    pub expires_at: Option<i64>,
}

/// A temp-ban waiting to be lifted.
#[derive(Debug, Clone)]
pub struct ScheduledUnban {
    pub platform: Platform,
    pub guild_id: String,
    pub user_id: String,
    pub unban_at: i64,
    pub case_number: i64,
    pub attempts: i64,
}

/// A mute (by role) waiting to be lifted.
#[derive(Debug, Clone)]
pub struct ScheduledUnmute {
    pub platform: Platform,
    pub guild_id: String,
    pub user_id: String,
    pub role_id: String,
    pub unmute_at: i64,
    pub case_number: i64,
    pub attempts: i64,
}

const CASE_COLUMNS: &str =
    "number, action, target_id, moderator_id, reason, created_at, expires_at, revoked_at";

fn case_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Case> {
    Ok(Case {
        number: r.get(0)?,
        action: r.get(1)?,
        target_id: r.get(2)?,
        moderator_id: r.get(3)?,
        reason: r.get(4)?,
        created_at: r.get(5)?,
        expires_at: r.get(6)?,
        revoked_at: r.get(7)?,
    })
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
    pub fn find_subscriptions(
        &self,
        kind: AlertKind,
        platform: Platform,
        scope: Scope<'_>,
        source: &str,
    ) -> DbResult<Vec<Subscription>> {
        let (col, val) = scope.column();
        self.query_subscriptions(
            &format!(
                "WHERE kind = ?1 AND platform = ?2 AND {col} = ?3
                   AND (source_id = ?4 OR lower(source_key) = lower(?4)
                        OR lower(source_name) = lower(?4))"
            ),
            params![kind.as_str(), platform_str(platform), val, source],
        )
    }

    pub fn delete_subscriptions(&self, ids: &[i64]) -> DbResult<usize> {
        let conn = self.conn();
        let mut n = 0;
        for id in ids {
            n += conn.execute("DELETE FROM subscriptions WHERE id = ?1", params![id])?;
        }
        Ok(n)
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
                    include_shorts, created_by, message, mention
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
                created_by: r.get(9)?,
                message: r.get(10)?,
                mention: r.get(11)?,
            }))
        })?;
        rows.filter_map(|r| r.transpose()).collect()
    }

    /// Turns Shorts on or off for the given subscriptions. Returns how many changed.
    pub fn set_include_shorts(&self, ids: &[i64], on: bool) -> DbResult<usize> {
        let conn = self.conn();
        let mut n = 0;
        for id in ids {
            n += conn.execute(
                "UPDATE subscriptions SET include_shorts = ?1 WHERE id = ?2",
                params![on as i64, id],
            )?;
        }
        Ok(n)
    }

    /// Sets (or with `None` clears) the custom alert text. Returns how many changed.
    pub fn set_alert_message(&self, ids: &[i64], message: Option<&str>) -> DbResult<usize> {
        self.update_subscriptions("message", ids, message)
    }

    /// Sets (or with `None` clears) the mention. Returns how many changed.
    pub fn set_alert_mention(&self, ids: &[i64], mention: Option<&str>) -> DbResult<usize> {
        self.update_subscriptions("mention", ids, mention)
    }

    fn update_subscriptions(
        &self,
        column: &'static str,
        ids: &[i64],
        value: Option<&str>,
    ) -> DbResult<usize> {
        let conn = self.conn();
        let sql = format!("UPDATE subscriptions SET {column} = ?1 WHERE id = ?2");
        let mut n = 0;
        for id in ids {
            n += conn.execute(&sql, params![value, id])?;
        }
        Ok(n)
    }

    /// The ID of the subscription posting `source_id` to `channel_id` on `platform`.
    pub fn subscription_id(
        &self,
        kind: AlertKind,
        source_id: &str,
        platform: Platform,
        channel_id: &str,
    ) -> DbResult<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT id FROM subscriptions
                 WHERE kind = ?1 AND source_id = ?2 AND platform = ?3 AND channel_id = ?4",
                params![kind.as_str(), source_id, platform_str(platform), channel_id],
                |r| r.get(0),
            )
            .optional()
    }

    /// Removes a server's alert subscriptions, admin/mod roles and the given settings, in one
    /// transaction (settings import with `--replace`). Returns the number of subscriptions removed.
    pub fn clear_server_config(
        &self,
        platform: Platform,
        guild_id: &str,
        setting_keys: &[&str],
    ) -> DbResult<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let p = platform_str(platform);
        let subs = tx.execute(
            "DELETE FROM subscriptions WHERE platform = ?1 AND guild_id = ?2",
            params![p, guild_id],
        )?;
        tx.execute(
            "DELETE FROM guild_roles WHERE platform = ?1 AND guild_id = ?2",
            params![p, guild_id],
        )?;
        tx.execute(
            "DELETE FROM custom_commands WHERE platform = ?1 AND guild_id = ?2",
            params![p, guild_id],
        )?;
        for key in setting_keys {
            tx.execute(
                "DELETE FROM guild_settings WHERE platform = ?1 AND guild_id = ?2 AND key = ?3",
                params![p, guild_id, key],
            )?;
        }
        tx.commit()?;
        Ok(subs)
    }

    /// Adds `(level, role_id)` pairs and sets settings in one transaction (settings import).
    pub fn apply_server_config(
        &self,
        platform: Platform,
        guild_id: &str,
        roles: &[(&str, String)],
        settings: &[(String, String)],
    ) -> DbResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let p = platform_str(platform);
        for (level, role) in roles {
            tx.execute(
                "INSERT OR IGNORE INTO guild_roles (platform, guild_id, level, role_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![p, guild_id, level, role],
            )?;
        }
        for (key, value) in settings {
            tx.execute(
                "INSERT OR REPLACE INTO guild_settings (platform, guild_id, key, value)
                 VALUES (?1, ?2, ?3, ?4)",
                params![p, guild_id, key, value],
            )?;
        }
        tx.commit()
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

    /// Marks a role as granting `level` ("admin" or "mod") in a server. False if it already did.
    pub fn add_guild_role(
        &self,
        platform: Platform,
        guild_id: &str,
        level: &str,
        role_id: &str,
    ) -> DbResult<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO guild_roles (platform, guild_id, level, role_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![platform_str(platform), guild_id, level, role_id],
        )?;
        Ok(n > 0)
    }

    pub fn remove_guild_role(
        &self,
        platform: Platform,
        guild_id: &str,
        level: &str,
        role_id: &str,
    ) -> DbResult<bool> {
        let n = self.conn().execute(
            "DELETE FROM guild_roles
             WHERE platform = ?1 AND guild_id = ?2 AND level = ?3 AND role_id = ?4",
            params![platform_str(platform), guild_id, level, role_id],
        )?;
        Ok(n > 0)
    }

    /// `(level, role_id)` pairs configured for a server.
    pub fn guild_roles(
        &self,
        platform: Platform,
        guild_id: &str,
    ) -> DbResult<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT level, role_id FROM guild_roles
             WHERE platform = ?1 AND guild_id = ?2 ORDER BY level, role_id",
        )?;
        let rows = stmt.query_map(params![platform_str(platform), guild_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect()
    }

    // --- moderation --------------------------------------------------------------------------

    /// Records a case and returns its number (1, 2, ... per server).
    pub fn add_case(&self, c: &NewCase<'_>) -> DbResult<i64> {
        let conn = self.conn();
        let p = platform_str(c.platform);
        conn.execute(
            "INSERT INTO mod_cases
               (platform, guild_id, number, action, target_id, moderator_id, reason, created_at,
                expires_at)
             SELECT ?1, ?2, COALESCE(MAX(number), 0) + 1, ?3, ?4, ?5, ?6, ?7, ?8
             FROM mod_cases WHERE platform = ?1 AND guild_id = ?2",
            params![
                p,
                c.guild_id,
                c.action,
                c.target_id,
                c.moderator_id,
                c.reason,
                now(),
                c.expires_at
            ],
        )?;
        conn.query_row(
            "SELECT MAX(number) FROM mod_cases WHERE platform = ?1 AND guild_id = ?2",
            params![p, c.guild_id],
            |r| r.get(0),
        )
    }

    pub fn case(&self, platform: Platform, guild_id: &str, number: i64) -> DbResult<Option<Case>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {CASE_COLUMNS} FROM mod_cases
                     WHERE platform = ?1 AND guild_id = ?2 AND number = ?3"
                ),
                params![platform_str(platform), guild_id, number],
                case_row,
            )
            .optional()
    }

    pub fn set_case_reason(
        &self,
        platform: Platform,
        guild_id: &str,
        number: i64,
        reason: &str,
    ) -> DbResult<bool> {
        let n = self.conn().execute(
            "UPDATE mod_cases SET reason = ?4
             WHERE platform = ?1 AND guild_id = ?2 AND number = ?3",
            params![platform_str(platform), guild_id, number, reason],
        )?;
        Ok(n > 0)
    }

    /// A user's cases in a server, newest first. `warnings_only`: warnings not removed.
    pub fn cases_for(
        &self,
        platform: Platform,
        guild_id: &str,
        user_id: &str,
        warnings_only: bool,
    ) -> DbResult<Vec<Case>> {
        let conn = self.conn();
        let filter = if warnings_only {
            "AND action = 'warn' AND revoked_at IS NULL"
        } else {
            ""
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT {CASE_COLUMNS} FROM mod_cases
             WHERE platform = ?1 AND guild_id = ?2 AND target_id = ?3 {filter}
             ORDER BY number DESC"
        ))?;
        let rows = stmt.query_map(params![platform_str(platform), guild_id, user_id], case_row)?;
        rows.collect()
    }

    /// Marks a warning as removed. False if the case isn't an active warning.
    pub fn revoke_warning(
        &self,
        platform: Platform,
        guild_id: &str,
        number: i64,
    ) -> DbResult<bool> {
        let n = self.conn().execute(
            "UPDATE mod_cases SET revoked_at = ?4
             WHERE platform = ?1 AND guild_id = ?2 AND number = ?3
               AND action = 'warn' AND revoked_at IS NULL",
            params![platform_str(platform), guild_id, number, now()],
        )?;
        Ok(n > 0)
    }

    /// Schedules (or reschedules) lifting a temp-ban.
    pub fn schedule_unban(
        &self,
        platform: Platform,
        guild_id: &str,
        user_id: &str,
        unban_at: i64,
        case_number: i64,
    ) -> DbResult<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO scheduled_unbans
               (platform, guild_id, user_id, unban_at, case_number, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![
                platform_str(platform),
                guild_id,
                user_id,
                unban_at,
                case_number
            ],
        )?;
        Ok(())
    }

    /// Drops a scheduled unban (the user was unbanned, or banned for good). False if none.
    pub fn cancel_unban(
        &self,
        platform: Platform,
        guild_id: &str,
        user_id: &str,
    ) -> DbResult<bool> {
        let n = self.conn().execute(
            "DELETE FROM scheduled_unbans WHERE platform = ?1 AND guild_id = ?2 AND user_id = ?3",
            params![platform_str(platform), guild_id, user_id],
        )?;
        Ok(n > 0)
    }

    pub fn scheduled_unban(
        &self,
        platform: Platform,
        guild_id: &str,
        user_id: &str,
    ) -> DbResult<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT unban_at FROM scheduled_unbans
                 WHERE platform = ?1 AND guild_id = ?2 AND user_id = ?3",
                params![platform_str(platform), guild_id, user_id],
                |r| r.get(0),
            )
            .optional()
    }

    /// Temp-bans whose time is up (`unban_at <= at`), oldest first.
    pub fn due_unbans(&self, at: i64) -> DbResult<Vec<ScheduledUnban>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT platform, guild_id, user_id, unban_at, case_number, attempts
             FROM scheduled_unbans WHERE unban_at <= ?1 ORDER BY unban_at",
        )?;
        let rows = stmt.query_map(params![at], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ScheduledUnban {
                    platform: Platform::Discord,
                    guild_id: r.get(1)?,
                    user_id: r.get(2)?,
                    unban_at: r.get(3)?,
                    case_number: r.get(4)?,
                    attempts: r.get(5)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (platform, mut u) = row?;
            if let Some(p) = parse_platform(&platform) {
                u.platform = p;
                out.push(u);
            }
        }
        Ok(out)
    }

    pub fn note_unban_attempt(
        &self,
        platform: Platform,
        guild_id: &str,
        user_id: &str,
    ) -> DbResult<()> {
        self.conn().execute(
            "UPDATE scheduled_unbans SET attempts = attempts + 1
             WHERE platform = ?1 AND guild_id = ?2 AND user_id = ?3",
            params![platform_str(platform), guild_id, user_id],
        )?;
        Ok(())
    }

    pub fn schedule_unmute(&self, u: &ScheduledUnmute) -> DbResult<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO scheduled_unmutes
               (platform, guild_id, user_id, role_id, unmute_at, case_number, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![
                platform_str(u.platform),
                u.guild_id,
                u.user_id,
                u.role_id,
                u.unmute_at,
                u.case_number
            ],
        )?;
        Ok(())
    }

    /// Returns true if a scheduled unmute was removed.
    pub fn cancel_unmute(
        &self,
        platform: Platform,
        guild_id: &str,
        user_id: &str,
    ) -> DbResult<bool> {
        let n = self.conn().execute(
            "DELETE FROM scheduled_unmutes WHERE platform = ?1 AND guild_id = ?2 AND user_id = ?3",
            params![platform_str(platform), guild_id, user_id],
        )?;
        Ok(n > 0)
    }

    pub fn due_unmutes(&self, at: i64) -> DbResult<Vec<ScheduledUnmute>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT platform, guild_id, user_id, role_id, unmute_at, case_number, attempts
             FROM scheduled_unmutes WHERE unmute_at <= ?1 ORDER BY unmute_at",
        )?;
        let rows = stmt.query_map(params![at], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ScheduledUnmute {
                    platform: Platform::Discord,
                    guild_id: r.get(1)?,
                    user_id: r.get(2)?,
                    role_id: r.get(3)?,
                    unmute_at: r.get(4)?,
                    case_number: r.get(5)?,
                    attempts: r.get(6)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (platform, mut u) = row?;
            if let Some(p) = parse_platform(&platform) {
                u.platform = p;
                out.push(u);
            }
        }
        Ok(out)
    }

    pub fn note_unmute_attempt(
        &self,
        platform: Platform,
        guild_id: &str,
        user_id: &str,
    ) -> DbResult<()> {
        self.conn().execute(
            "UPDATE scheduled_unmutes SET attempts = attempts + 1
             WHERE platform = ?1 AND guild_id = ?2 AND user_id = ?3",
            params![platform_str(platform), guild_id, user_id],
        )?;
        Ok(())
    }

    pub fn guild_setting(
        &self,
        platform: Platform,
        guild_id: &str,
        key: &str,
    ) -> DbResult<Option<String>> {
        self.conn()
            .query_row(
                "SELECT value FROM guild_settings
                 WHERE platform = ?1 AND guild_id = ?2 AND key = ?3",
                params![platform_str(platform), guild_id, key],
                |r| r.get(0),
            )
            .optional()
    }

    /// A custom command's text.
    pub fn custom_command(
        &self,
        platform: Platform,
        guild_id: &str,
        name: &str,
    ) -> DbResult<Option<String>> {
        self.conn()
            .query_row(
                "SELECT response FROM custom_commands
                 WHERE platform = ?1 AND guild_id = ?2 AND name = ?3",
                params![platform_str(platform), guild_id, name],
                |r| r.get(0),
            )
            .optional()
    }

    /// Adds or replaces a custom command.
    pub fn set_custom_command(
        &self,
        platform: Platform,
        guild_id: &str,
        name: &str,
        response: &str,
        user_id: &str,
    ) -> DbResult<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO custom_commands
               (platform, guild_id, name, response, created_by, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                platform_str(platform),
                guild_id,
                name,
                response,
                user_id,
                now()
            ],
        )?;
        Ok(())
    }

    /// Returns true if it existed.
    pub fn delete_custom_command(
        &self,
        platform: Platform,
        guild_id: &str,
        name: &str,
    ) -> DbResult<bool> {
        let n = self.conn().execute(
            "DELETE FROM custom_commands WHERE platform = ?1 AND guild_id = ?2 AND name = ?3",
            params![platform_str(platform), guild_id, name],
        )?;
        Ok(n > 0)
    }

    /// A server's custom commands as `(name, text)`, by name.
    pub fn custom_commands(
        &self,
        platform: Platform,
        guild_id: &str,
    ) -> DbResult<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT name, response FROM custom_commands
             WHERE platform = ?1 AND guild_id = ?2 ORDER BY name",
        )?;
        let rows = stmt.query_map(params![platform_str(platform), guild_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect()
    }

    /// Sets a server setting; `None` removes it.
    pub fn set_guild_setting(
        &self,
        platform: Platform,
        guild_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> DbResult<()> {
        let conn = self.conn();
        let p = platform_str(platform);
        match value {
            Some(v) => conn.execute(
                "INSERT OR REPLACE INTO guild_settings (platform, guild_id, key, value)
                 VALUES (?1, ?2, ?3, ?4)",
                params![p, guild_id, key, v],
            )?,
            None => conn.execute(
                "DELETE FROM guild_settings WHERE platform = ?1 AND guild_id = ?2 AND key = ?3",
                params![p, guild_id, key],
            )?,
        };
        Ok(())
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

        let found = store
            .find_subscriptions(AlertKind::YouTube, Platform::Discord, scope, "some channel")
            .unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|s| s.created_by == "u1"));
        assert!(store
            .find_subscriptions(AlertKind::YouTube, Platform::Fluxer, scope, "UC1")
            .unwrap()
            .is_empty());
        let ids: Vec<i64> = found.iter().map(|s| s.id).collect();
        assert_eq!(store.delete_subscriptions(&ids).unwrap(), 2);
        assert!(store
            .all_subscriptions(AlertKind::YouTube)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn scheduled_unmutes() {
        let store = Store::open_in_memory().unwrap();
        let u = ScheduledUnmute {
            platform: Platform::Fluxer,
            guild_id: "g".into(),
            user_id: "u".into(),
            role_id: "r".into(),
            unmute_at: 100,
            case_number: 3,
            attempts: 0,
        };
        store.schedule_unmute(&u).unwrap();
        assert!(store.due_unmutes(99).unwrap().is_empty());
        let due = store.due_unmutes(100).unwrap();
        assert_eq!((due.len(), due[0].role_id.as_str()), (1, "r"));
        assert_eq!(due[0].platform, Platform::Fluxer);
        store
            .note_unmute_attempt(Platform::Fluxer, "g", "u")
            .unwrap();
        assert_eq!(store.due_unmutes(100).unwrap()[0].attempts, 1);
        assert!(store.cancel_unmute(Platform::Fluxer, "g", "u").unwrap());
        assert!(!store.cancel_unmute(Platform::Fluxer, "g", "u").unwrap());
    }

    #[test]
    fn alert_message_and_mention() {
        let store = Store::open_in_memory().unwrap();
        store.add_subscription(&sub("UC1", "c1")).unwrap();
        let get = || {
            store
                .all_subscriptions(AlertKind::YouTube)
                .unwrap()
                .remove(0)
        };
        let id = get().id;
        assert_eq!((get().message, get().mention), (None, None));
        assert_eq!(store.set_alert_message(&[id], Some("{name}!")).unwrap(), 1);
        assert_eq!(store.set_alert_mention(&[id], Some("everyone")).unwrap(), 1);
        assert_eq!(get().message.as_deref(), Some("{name}!"));
        assert_eq!(get().mention.as_deref(), Some("everyone"));
        store.set_alert_message(&[id], None).unwrap();
        store.set_alert_mention(&[id], None).unwrap();
        assert_eq!((get().message, get().mention), (None, None));
    }

    #[test]
    fn shorts_toggle_and_pending_items() {
        let store = Store::open_in_memory().unwrap();
        store.add_subscription(&sub("UC1", "c1")).unwrap();
        let get = || store.all_subscriptions(AlertKind::YouTube).unwrap()[0].include_shorts;
        assert!(!get(), "Shorts are off by default");
        let id = store.all_subscriptions(AlertKind::YouTube).unwrap()[0].id;
        assert_eq!(store.set_include_shorts(&[id], true).unwrap(), 1);
        assert!(get());
        assert_eq!(store.set_include_shorts(&[id + 100], false).unwrap(), 0);
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

    fn case<'a>(guild: &'a str, action: &'a str, target: &'a str) -> NewCase<'a> {
        NewCase {
            platform: Platform::Discord,
            guild_id: guild,
            action,
            target_id: target,
            moderator_id: "mod",
            reason: Some("spam"),
            expires_at: None,
        }
    }

    #[test]
    fn cases_are_numbered_per_server() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.add_case(&case("g1", "warn", "u1")).unwrap(), 1);
        assert_eq!(store.add_case(&case("g1", "kick", "u1")).unwrap(), 2);
        assert_eq!(store.add_case(&case("g2", "warn", "u1")).unwrap(), 1);
        let mut fluxer = case("g1", "warn", "u1");
        fluxer.platform = Platform::Fluxer;
        assert_eq!(
            store.add_case(&fluxer).unwrap(),
            1,
            "own count per platform"
        );

        let c = store.case(Platform::Discord, "g1", 2).unwrap().unwrap();
        assert_eq!((c.action.as_str(), c.target_id.as_str()), ("kick", "u1"));
        assert!(store.case(Platform::Discord, "g1", 3).unwrap().is_none());

        assert!(store
            .set_case_reason(Platform::Discord, "g1", 2, "flood")
            .unwrap());
        let c = store.case(Platform::Discord, "g1", 2).unwrap().unwrap();
        assert_eq!(c.reason.as_deref(), Some("flood"));

        let all = store
            .cases_for(Platform::Discord, "g1", "u1", false)
            .unwrap();
        assert_eq!(all.iter().map(|c| c.number).collect::<Vec<_>>(), [2, 1]);
    }

    #[test]
    fn warnings_can_be_revoked_once() {
        let store = Store::open_in_memory().unwrap();
        store.add_case(&case("g1", "warn", "u1")).unwrap();
        store.add_case(&case("g1", "kick", "u1")).unwrap();
        store.add_case(&case("g1", "warn", "u1")).unwrap();
        let warns = |s: &Store| {
            s.cases_for(Platform::Discord, "g1", "u1", true)
                .unwrap()
                .len()
        };
        assert_eq!(warns(&store), 2);
        assert!(
            !store.revoke_warning(Platform::Discord, "g1", 2).unwrap(),
            "not a warning"
        );
        assert!(store.revoke_warning(Platform::Discord, "g1", 1).unwrap());
        assert!(
            !store.revoke_warning(Platform::Discord, "g1", 1).unwrap(),
            "already removed"
        );
        assert_eq!(warns(&store), 1);
    }

    #[test]
    fn scheduled_unbans() {
        let store = Store::open_in_memory().unwrap();
        store
            .schedule_unban(Platform::Discord, "g1", "u1", 100, 1)
            .unwrap();
        store
            .schedule_unban(Platform::Fluxer, "g1", "u2", 50, 2)
            .unwrap();
        store
            .schedule_unban(Platform::Discord, "g1", "u3", 500, 3)
            .unwrap();
        // Rescheduling replaces.
        store
            .schedule_unban(Platform::Discord, "g1", "u3", 300, 4)
            .unwrap();

        let due = store.due_unbans(300).unwrap();
        let users: Vec<_> = due.iter().map(|u| u.user_id.as_str()).collect();
        assert_eq!(users, ["u2", "u1", "u3"]);
        assert_eq!(due[0].platform, Platform::Fluxer);
        assert_eq!(due[2].case_number, 4);

        store
            .note_unban_attempt(Platform::Discord, "g1", "u1")
            .unwrap();
        assert_eq!(store.due_unbans(100).unwrap()[1].attempts, 1);
        assert!(store.cancel_unban(Platform::Discord, "g1", "u1").unwrap());
        assert!(!store.cancel_unban(Platform::Discord, "g1", "u1").unwrap());
        assert_eq!(
            store
                .scheduled_unban(Platform::Discord, "g1", "u3")
                .unwrap(),
            Some(300)
        );
        assert_eq!(store.due_unbans(299).unwrap().len(), 1);
    }

    #[test]
    fn guild_settings() {
        let store = Store::open_in_memory().unwrap();
        let get = |s: &Store| {
            s.guild_setting(Platform::Discord, "g1", "modlog_channel")
                .unwrap()
        };
        assert_eq!(get(&store), None);
        store
            .set_guild_setting(Platform::Discord, "g1", "modlog_channel", Some("c1"))
            .unwrap();
        store
            .set_guild_setting(Platform::Discord, "g1", "modlog_channel", Some("c2"))
            .unwrap();
        assert_eq!(get(&store).as_deref(), Some("c2"));
        store
            .set_guild_setting(Platform::Discord, "g1", "modlog_channel", None)
            .unwrap();
        assert_eq!(get(&store), None);
    }
}
