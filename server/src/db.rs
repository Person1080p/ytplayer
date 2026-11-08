use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{now, Track};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tracks (
    video_id   TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    artist     TEXT NOT NULL,
    album      TEXT,
    duration   INTEGER,
    thumbnail  TEXT,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS plex_matches (
    video_id   TEXT PRIMARY KEY,
    matched    INTEGER NOT NULL,
    rating_key TEXT,
    part_key   TEXT,
    codec      TEXT,
    title      TEXT,
    artist     TEXT,
    checked_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    video_id    TEXT NOT NULL,
    played_at   INTEGER NOT NULL,
    source      TEXT NOT NULL,
    scrobbled   INTEGER NOT NULL DEFAULT 0,
    yt_reported INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS history_played_at ON history(played_at DESC);

CREATE TABLE IF NOT EXISTS playlists (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS playlist_items (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    pos         INTEGER NOT NULL,
    video_id    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS playlist_items_pl ON playlist_items(playlist_id, pos);

-- YouTube Music playlists and albums linked from the Playlists page (opened live, not copied).
CREATE TABLE IF NOT EXISTS linked_playlists (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    subtitle   TEXT,
    thumbnail  TEXT,
    added_at   INTEGER NOT NULL
);

-- Spotify playlists imported from a link: the list as last synced from Spotify (JSON).
CREATE TABLE IF NOT EXISTS spotify_playlists (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    owner      TEXT,
    thumbnail  TEXT,
    count      INTEGER NOT NULL,
    data       TEXT NOT NULL,
    synced_at  INTEGER NOT NULL,
    added_at   INTEGER NOT NULL
);

-- Songs with no audio anywhere (no Plex copy, no YouTube match, or the video is gone).
CREATE TABLE IF NOT EXISTS missing_audio (
    video_id TEXT PRIMARY KEY,
    reason   TEXT NOT NULL,
    at       INTEGER NOT NULL
);

-- A YouTube song played from another video you picked (the automatic one was wrong).
CREATE TABLE IF NOT EXISTS video_overrides (
    video_id  TEXT PRIMARY KEY,
    target_id TEXT NOT NULL,
    at        INTEGER NOT NULL
);

-- Album art of Spotify songs, looked up when each first plays (the embed page has none).
CREATE TABLE IF NOT EXISTS spotify_art (
    track_id TEXT PRIMARY KEY,
    url      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS liked (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    provider    TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    title       TEXT NOT NULL,
    artist      TEXT NOT NULL,
    album       TEXT,
    duration    INTEGER,
    video_id    TEXT,
    status      TEXT NOT NULL,
    added_at    INTEGER NOT NULL,
    UNIQUE(provider, provider_id)
);

CREATE TABLE IF NOT EXISTS import_jobs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    provider    TEXT NOT NULL,
    status      TEXT NOT NULL,
    total       INTEGER NOT NULL DEFAULT 0,
    done        INTEGER NOT NULL DEFAULT 0,
    matched     INTEGER NOT NULL DEFAULT 0,
    error       TEXT,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER
);

-- Two-way Last.fm loves sync: songs that were on both sides after the last sync.
-- lf_seen = 0 for songs we loved on Last.fm but haven't yet seen in its loved list.
CREATE TABLE IF NOT EXISTS lastfm_sync (
    key     TEXT PRIMARY KEY,
    lf_seen INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS api_cache (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS lookup_cache (
    key        TEXT PRIMARY KEY,
    video_id   TEXT,
    created_at INTEGER NOT NULL
);
"#;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(SCHEMA)?;
        // Migration: play history is kept per listener profile.
        let has_profile: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('history') WHERE name='profile'")?
            .exists([])?;
        if !has_profile {
            conn.execute_batch(
                "ALTER TABLE history ADD COLUMN profile TEXT NOT NULL DEFAULT 'main';
                 CREATE INDEX IF NOT EXISTS history_profile ON history(profile, played_at DESC);",
            )?;
        }
        // Migration: a human-readable summary line per import job.
        let has_note: bool = conn.prepare("SELECT 1 FROM pragma_table_info('import_jobs') WHERE name='note'")?.exists([])?;
        if !has_note {
            conn.execute_batch("ALTER TABLE import_jobs ADD COLUMN note TEXT;")?;
        }
        // Migration: Plex matches you chose (kept as they are), and which matcher made the rest.
        let has_manual: bool = conn.prepare("SELECT 1 FROM pragma_table_info('plex_matches') WHERE name='manual'")?.exists([])?;
        if !has_manual {
            conn.execute_batch(
                "ALTER TABLE plex_matches ADD COLUMN manual INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE plex_matches ADD COLUMN version INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        // Matching now also looks at YouTube videos: give songs it found nothing for before
        // another look (once).
        let resolver_v: Option<String> =
            conn.query_row("SELECT value FROM settings WHERE key='resolver.version'", [], |r| r.get(0)).optional()?;
        if resolver_v.as_deref() != Some("2") {
            conn.execute("DELETE FROM lookup_cache WHERE video_id IS NULL", [])?;
            conn.execute("INSERT OR REPLACE INTO settings(key, value) VALUES('resolver.version', '2')", [])?;
        }
        // Spotify app keys and login tokens from the removed Spotify API support.
        conn.execute("DELETE FROM settings WHERE key LIKE 'spotifyapp.%' OR key LIKE 'spotify.%'", [])?;
        // Jobs that were running when the process died will never finish.
        conn.execute(
            "UPDATE import_jobs SET status='failed', error='server restarted', finished_at=?1 WHERE status='running'",
            params![now()],
        )?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Run a closure against the connection on the blocking pool.
    pub async fn call<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut c = conn.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut c)
        })
        .await?;
        Ok(res?)
    }

    // ---- settings -------------------------------------------------------

    pub async fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let key = key.to_string();
        self.call(move |c| {
            c.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()
        })
        .await
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let (key, value) = (key.to_string(), value.to_string());
        self.call(move |c| {
            c.execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn delete_settings(&self, prefix: &str) -> anyhow::Result<()> {
        let pattern = format!("{prefix}%");
        self.call(move |c| c.execute("DELETE FROM settings WHERE key LIKE ?1", [pattern]).map(|_| ()))
            .await
    }

    /// The active listener profile ("main" or "fresh").
    pub async fn active_profile(&self) -> String {
        self.get_setting(crate::services::profile::PROFILE_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::services::profile::MAIN.to_string())
    }

    // ---- response cache -----------------------------------------------------

    /// Cached value and whether it's still fresh.
    pub async fn cache_get(&self, key: &str) -> anyhow::Result<Option<(String, bool)>> {
        let key = key.to_string();
        self.call(move |c| {
            c.query_row("SELECT value, expires_at FROM api_cache WHERE key=?1", [key], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? > now()))
            })
            .optional()
        })
        .await
    }

    pub async fn cache_put(&self, key: &str, value: &str, ttl_secs: i64) -> anyhow::Result<()> {
        let (key, value) = (key.to_string(), value.to_string());
        self.call(move |c| {
            c.execute(
                "INSERT INTO api_cache(key,value,expires_at) VALUES(?1,?2,?3)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value, expires_at=excluded.expires_at",
                params![key, value, now() + ttl_secs],
            )
            .map(|_| ())
        })
        .await
    }

    /// Drop entries that expired more than a week ago (stale entries are kept a while as a
    /// fallback for when YouTube errors).
    pub async fn cache_purge(&self) -> anyhow::Result<usize> {
        self.call(|c| c.execute("DELETE FROM api_cache WHERE expires_at < ?1", [now() - 7 * 86400])).await
    }

    pub async fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.get_setting(key).await {
            Ok(Some(v)) => v == "1" || v == "true",
            _ => default,
        }
    }

    // ---- tracks ---------------------------------------------------------

    pub async fn upsert_tracks(&self, tracks: Vec<Track>) -> anyhow::Result<()> {
        if tracks.is_empty() {
            return Ok(());
        }
        self.call(move |c| {
            let tx = c.transaction()?;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO tracks(video_id,title,artist,album,duration,thumbnail,updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(video_id) DO UPDATE SET
                       title=excluded.title,
                       artist=CASE WHEN excluded.artist<>'' THEN excluded.artist ELSE tracks.artist END,
                       album=COALESCE(excluded.album, tracks.album),
                       duration=COALESCE(excluded.duration, tracks.duration),
                       thumbnail=COALESCE(excluded.thumbnail, tracks.thumbnail),
                       updated_at=excluded.updated_at",
                )?;
                let ts = now();
                for t in &tracks {
                    stmt.execute(params![t.video_id, t.title, t.artist, t.album, t.duration, t.thumbnail, ts])?;
                }
            }
            tx.commit()
        })
        .await
    }

    pub async fn get_track(&self, video_id: &str) -> anyhow::Result<Option<Track>> {
        let id = video_id.to_string();
        self.call(move |c| {
            c.query_row(
                "SELECT video_id,title,artist,album,duration,thumbnail FROM tracks WHERE video_id=?1",
                [id],
                row_to_track,
            )
            .optional()
        })
        .await
    }

    /// Recently played tracks of the active profile.
    pub async fn recent_video_ids(&self, limit: u32) -> anyhow::Result<Vec<String>> {
        let profile = self.active_profile().await;
        self.call(move |c| {
            let mut stmt = c.prepare(
                "SELECT video_id FROM history WHERE profile=?1 ORDER BY played_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![profile, limit], |r| r.get(0))?;
            rows.collect()
        })
        .await
    }
}

pub fn row_to_track(r: &rusqlite::Row) -> rusqlite::Result<Track> {
    Ok(Track {
        video_id: r.get(0)?,
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        duration: r.get(4)?,
        thumbnail: r.get(5)?,
    })
}
