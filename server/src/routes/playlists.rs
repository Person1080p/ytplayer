//! Locally saved playlists ("saved queues").

use axum::{
    extract::{Path, State},
    Json,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    db::row_to_track,
    error::{ApiResult, AppError},
    models::{now, Track},
    state::SharedState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    id: i64,
    name: String,
    count: i64,
    updated_at: i64,
    thumbnail: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    id: i64,
    name: String,
    tracks: Vec<Track>,
}

pub async fn list(State(st): State<SharedState>) -> ApiResult<Vec<PlaylistSummary>> {
    let rows = st
        .db
        .call(|c| {
            let mut stmt = c.prepare(
                "SELECT p.id, p.name, p.updated_at,
                        (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id=p.id),
                        (SELECT t.thumbnail FROM playlist_items i JOIN tracks t ON t.video_id=i.video_id
                          WHERE i.playlist_id=p.id ORDER BY i.pos LIMIT 1)
                 FROM playlists p ORDER BY p.updated_at DESC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(PlaylistSummary { id: r.get(0)?, name: r.get(1)?, updated_at: r.get(2)?, count: r.get(3)?, thumbnail: r.get(4)? })
            })?;
            rows.collect()
        })
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct CreateBody {
    name: String,
    #[serde(default)]
    tracks: Vec<Track>,
}

pub async fn create(State(st): State<SharedState>, Json(b): Json<CreateBody>) -> ApiResult<Playlist> {
    let name = b.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad("name required"));
    }
    st.db.upsert_tracks(b.tracks.clone()).await?;
    let ids: Vec<String> = b.tracks.iter().map(|t| t.video_id.clone()).collect();
    let n = name.clone();
    let id = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("INSERT INTO playlists(name, created_at, updated_at) VALUES(?1,?2,?2)", params![n, now()])?;
            let id = tx.last_insert_rowid();
            for (i, v) in ids.iter().enumerate() {
                tx.execute("INSERT INTO playlist_items(playlist_id,pos,video_id) VALUES(?1,?2,?3)", params![id, i as i64, v])?;
            }
            tx.commit()?;
            Ok(id)
        })
        .await?;
    Ok(Json(Playlist { id, name, tracks: b.tracks }))
}

pub async fn get(State(st): State<SharedState>, Path(id): Path<i64>) -> ApiResult<Playlist> {
    let res = st
        .db
        .call(move |c| {
            let Some(name) = c.query_row("SELECT name FROM playlists WHERE id=?1", [id], |r| r.get::<_, String>(0)).optional()? else {
                return Ok(None);
            };
            let mut stmt = c.prepare(
                "SELECT t.video_id,t.title,t.artist,t.album,t.duration,t.thumbnail
                 FROM playlist_items i JOIN tracks t ON t.video_id=i.video_id
                 WHERE i.playlist_id=?1 ORDER BY i.pos",
            )?;
            let tracks = stmt.query_map([id], row_to_track)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some(Playlist { id, name, tracks }))
        })
        .await?;
    res.map(Json).ok_or(AppError::NotFound)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    /// Full replacement of the track list (ordering/removal).
    #[serde(default)]
    tracks: Option<Vec<Track>>,
    /// Tracks appended to the end.
    #[serde(default)]
    append: Option<Vec<Track>>,
}

pub async fn update(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<UpdateBody>) -> ApiResult<Playlist> {
    let mut all = b.tracks.clone().unwrap_or_default();
    all.extend(b.append.clone().unwrap_or_default());
    st.db.upsert_tracks(all).await?;
    st.db
        .call(move |c| {
            let tx = c.transaction()?;
            if let Some(n) = b.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                tx.execute("UPDATE playlists SET name=?1 WHERE id=?2", params![n, id])?;
            }
            if let Some(tracks) = &b.tracks {
                tx.execute("DELETE FROM playlist_items WHERE playlist_id=?1", [id])?;
                for (i, t) in tracks.iter().enumerate() {
                    tx.execute("INSERT INTO playlist_items(playlist_id,pos,video_id) VALUES(?1,?2,?3)", params![id, i as i64, t.video_id])?;
                }
            }
            if let Some(append) = &b.append {
                let start: i64 = tx.query_row("SELECT COALESCE(MAX(pos)+1,0) FROM playlist_items WHERE playlist_id=?1", [id], |r| r.get(0))?;
                for (i, t) in append.iter().enumerate() {
                    tx.execute("INSERT INTO playlist_items(playlist_id,pos,video_id) VALUES(?1,?2,?3)", params![id, start + i as i64, t.video_id])?;
                }
            }
            tx.execute("UPDATE playlists SET updated_at=?1 WHERE id=?2", params![now(), id])?;
            tx.commit()
        })
        .await?;
    get(State(st), Path(id)).await
}

pub async fn delete(State(st): State<SharedState>, Path(id): Path<i64>) -> ApiResult<serde_json::Value> {
    st.db.call(move |c| c.execute("DELETE FROM playlists WHERE id=?1", [id])).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- linked YouTube Music playlists -----------------------------------------------------

/// A YouTube Music playlist or album shown on the Playlists page. Its tracks are loaded from
/// YouTube Music when opened, so it stays up to date (unlike saving a copy).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedPlaylist {
    /// Playlist id (`PL…`, `VL…`, `RD…`) or album browse id (`MPRE…`).
    id: String,
    title: String,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    added_at: i64,
}

pub async fn linked(State(st): State<SharedState>) -> ApiResult<Vec<LinkedPlaylist>> {
    let rows = st
        .db
        .call(|c| {
            let mut stmt = c.prepare("SELECT id, title, subtitle, thumbnail, added_at FROM linked_playlists ORDER BY added_at DESC")?;
            let rows = stmt.query_map([], |r| {
                Ok(LinkedPlaylist { id: r.get(0)?, title: r.get(1)?, subtitle: r.get(2)?, thumbnail: r.get(3)?, added_at: r.get(4)? })
            })?;
            rows.collect()
        })
        .await?;
    Ok(Json(rows))
}

pub async fn link(State(st): State<SharedState>, Json(mut p): Json<LinkedPlaylist>) -> ApiResult<LinkedPlaylist> {
    if p.id.is_empty() || p.id.len() > 128 || !p.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(AppError::bad("invalid playlist id"));
    }
    p.title = p.title.trim().to_string();
    if p.title.is_empty() {
        p.title = "Playlist".into();
    }
    p.added_at = now();
    let (id, title, subtitle, thumbnail, at) = (p.id.clone(), p.title.clone(), p.subtitle.clone(), p.thumbnail.clone(), p.added_at);
    st.db
        .call(move |c| {
            c.execute(
                "INSERT INTO linked_playlists(id, title, subtitle, thumbnail, added_at) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(id) DO UPDATE SET title=excluded.title, subtitle=excluded.subtitle, thumbnail=excluded.thumbnail",
                params![id, title, subtitle, thumbnail, at],
            )
        })
        .await?;
    Ok(Json(p))
}

pub async fn unlink(State(st): State<SharedState>, Path(id): Path<String>) -> ApiResult<serde_json::Value> {
    st.db.call(move |c| c.execute("DELETE FROM linked_playlists WHERE id=?1", [id])).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- Spotify playlists ----------------------------------------------------------------------

use crate::services::spotify::{self, SpotifyPlaylist, SpotifySummary};

pub async fn spotify_list(State(st): State<SharedState>) -> ApiResult<Vec<SpotifySummary>> {
    Ok(Json(spotify::list(&st).await?))
}

#[derive(Deserialize)]
pub struct SpotifyImportBody {
    url: String,
}

/// Import a Spotify playlist from its link. Reads the list from Spotify only: songs are matched
/// to audio when they play.
pub async fn spotify_import(State(st): State<SharedState>, Json(b): Json<SpotifyImportBody>) -> ApiResult<SpotifyPlaylist> {
    if spotify::playlist_id(&b.url).is_none() {
        return Err(AppError::bad("That isn't a Spotify playlist link (open.spotify.com/playlist/…)"));
    }
    spotify::import(&st, &b.url).await.map(Json).map_err(|e| AppError::bad(format!("{e:#}")))
}

#[derive(Deserialize)]
pub struct SpotifyOpenQuery {
    #[serde(default)]
    refresh: Option<String>,
}

/// A Spotify playlist: as stored, or synced with Spotify again first when that was a while ago.
pub async fn spotify_get(
    State(st): State<SharedState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<SpotifyOpenQuery>,
) -> ApiResult<SpotifyPlaylist> {
    spotify::open(&st, &id, q.refresh.is_some()).await?.map(Json).ok_or(AppError::NotFound)
}

pub async fn spotify_remove(State(st): State<SharedState>, Path(id): Path<String>) -> ApiResult<serde_json::Value> {
    spotify::remove(&st, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SpotifyExportBody {
    csv: String,
}

/// The full list of a Spotify playlist from an Exportify CSV (songs past the first 100).
pub async fn spotify_add_export(
    State(st): State<SharedState>,
    Path(id): Path<String>,
    Json(b): Json<SpotifyExportBody>,
) -> ApiResult<SpotifyPlaylist> {
    spotify::add_export(&st, &id, &b.csv).await.map(Json).map_err(|e| AppError::bad(format!("{e:#}")))
}

#[derive(Deserialize)]
pub struct ExportDirBody {
    /// Folder(s) to watch for Exportify downloads (`;`-separated); empty: the Downloads folders.
    dir: String,
}

pub async fn spotify_set_export_dir(State(st): State<SharedState>, Json(b): Json<ExportDirBody>) -> ApiResult<serde_json::Value> {
    let dir = b.dir.trim();
    if dir.is_empty() {
        st.db.delete_settings(spotify::EXPORT_DIR_KEY).await?;
    } else {
        if let Some(missing) = dir.split(';').map(str::trim).find(|d| !d.is_empty() && !std::path::Path::new(d).is_dir()) {
            return Err(AppError::bad(format!("The server can't find the folder {missing}")));
        }
        st.db.set_setting(spotify::EXPORT_DIR_KEY, dir).await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}
