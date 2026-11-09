//! Liked-track library and imports.

use axum::{
    extract::{Query, State},
    Json,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{
    error::{ApiResult, AppError},
    models::{now, Track},
    services::{fileimport, importers, resolver},
    state::SharedState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LikedEntry {
    id: i64,
    provider: String,
    title: String,
    artist: String,
    album: Option<String>,
    status: String,
    track: Option<Track>,
}

#[derive(Deserialize)]
pub struct LikedQuery {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

pub async fn liked(State(st): State<SharedState>, Query(q): Query<LikedQuery>) -> ApiResult<Vec<LikedEntry>> {
    let rows = st
        .db
        .call(move |c| {
            let like = q.q.as_deref().map(|s| format!("%{}%", s.trim())).unwrap_or_else(|| "%".into());
            let mut stmt = c.prepare(
                "SELECT l.id, l.provider, l.title, l.artist, l.album, l.status,
                        t.video_id, t.title, t.artist, t.album, t.duration, t.thumbnail
                 FROM liked l LEFT JOIN tracks t ON t.video_id = l.video_id
                 WHERE (?1 IS NULL OR l.provider = ?1)
                   AND (?2 IS NULL OR l.status = ?2 OR (?2 = 'unmatched' AND l.status = 'error'))
                   AND (l.title LIKE ?3 OR l.artist LIKE ?3)
                 ORDER BY l.added_at DESC, l.id DESC
                 LIMIT 5000",
            )?;
            let rows = stmt.query_map(params![q.provider, q.status, like], |r| {
                let vid: Option<String> = r.get(6)?;
                let track = match vid {
                    Some(video_id) => Some(Track {
                        video_id,
                        title: r.get(7)?,
                        artist: r.get(8)?,
                        album: r.get(9)?,
                        duration: r.get(10)?,
                        thumbnail: r.get(11)?,
                    }),
                    None => None,
                };
                Ok(LikedEntry {
                    id: r.get(0)?,
                    provider: r.get(1)?,
                    title: r.get(2)?,
                    artist: r.get(3)?,
                    album: r.get(4)?,
                    status: r.get(5)?,
                    track,
                })
            })?;
            rows.collect()
        })
        .await?;
    Ok(Json(rows))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    id: i64,
    provider: String,
    status: String,
    total: i64,
    done: i64,
    matched: i64,
    error: Option<String>,
    note: Option<String>,
    started_at: i64,
    finished_at: Option<i64>,
}

pub async fn jobs(State(st): State<SharedState>) -> ApiResult<Vec<Job>> {
    let rows = st
        .db
        .call(|c| {
            let mut stmt = c.prepare(
                "SELECT id,provider,status,total,done,matched,error,started_at,finished_at,note FROM import_jobs ORDER BY id DESC LIMIT 20",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(Job {
                    id: r.get(0)?,
                    provider: r.get(1)?,
                    status: r.get(2)?,
                    total: r.get(3)?,
                    done: r.get(4)?,
                    matched: r.get(5)?,
                    error: r.get(6)?,
                    started_at: r.get(7)?,
                    finished_at: r.get(8)?,
                    note: r.get(9)?,
                })
            })?;
            rows.collect()
        })
        .await?;
    Ok(Json(rows))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStarted {
    job_id: i64,
}

pub async fn import_ytmusic(State(st): State<SharedState>) -> ApiResult<JobStarted> {
    if !crate::services::sources::youtube_enabled(&st.db).await {
        return Err(AppError::NotConfigured("YouTube is turned off in Settings → Audio sources".into()));
    }
    if !st.yt.is_authenticated().await {
        return Err(AppError::NotConfigured("Add your YouTube Music cookie in Settings first".into()));
    }
    let job_id = importers::start(st.clone(), "ytmusic", importers::ytmusic_liked(st.clone())).await?;
    Ok(Json(JobStarted { job_id }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeezerBody {
    user_id: String,
    #[serde(default)]
    access_token: Option<String>,
}

pub async fn import_deezer(State(st): State<SharedState>, Json(b): Json<DeezerBody>) -> ApiResult<JobStarted> {
    let user_id = b.user_id.trim().trim_start_matches("https://www.deezer.com/").to_string();
    // Accept a profile URL like https://www.deezer.com/en/profile/12345
    let user_id = user_id.rsplit('/').next().unwrap_or(&user_id).to_string();
    if user_id.is_empty() {
        return Err(AppError::bad("Deezer user id required"));
    }
    st.db.set_setting("deezer.user_id", &user_id).await?;
    let token = b.access_token.filter(|t| !t.trim().is_empty());
    let job_id = importers::start(st.clone(), "deezer", importers::deezer_favorites(st.http.clone(), user_id, token)).await?;
    Ok(Json(JobStarted { job_id }))
}

#[derive(Deserialize)]
pub struct TextBody {
    content: String,
    #[serde(default)]
    provider: Option<String>,
    /// Don't look songs up on YouTube now: each is matched when it first plays.
    #[serde(default)]
    lazy: bool,
}

pub async fn import_text(State(st): State<SharedState>, Json(b): Json<TextBody>) -> ApiResult<JobStarted> {
    let items = importers::parse_text(&b.content)?;
    if items.is_empty() {
        return Err(AppError::bad("No tracks found. Use a CSV export or 'Artist - Title' lines."));
    }
    let provider = b.provider.filter(|p| !p.is_empty()).unwrap_or_else(|| "file".into());
    let job_id = importers::start_with(st.clone(), &provider, b.lazy, async move { Ok(items) }).await?;
    Ok(Json(JobStarted { job_id }))
}

// ---- arbitrary export files (Demus, JSON, plist, zip, M3U, CSV, ...) ---------------------

#[derive(Deserialize)]
pub struct FileQuery {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Don't look songs up on YouTube now: each is matched when it first plays.
    #[serde(default)]
    lazy: bool,
}

pub async fn import_file(
    State(st): State<SharedState>,
    Query(q): Query<FileQuery>,
    body: axum::body::Bytes,
) -> ApiResult<JobStarted> {
    let name = q.name.unwrap_or_else(|| "upload".into());
    let items = fileimport::parse(&body, &name).map_err(|e| AppError::bad(format!("{e:#}")))?;
    let provider = q.provider.filter(|p| !p.is_empty()).unwrap_or_else(|| "file".into());
    let job_id = importers::start_with(st.clone(), &provider, q.lazy, async move { Ok(items) }).await?;
    Ok(Json(JobStarted { job_id }))
}

// ---- managing the library -----------------------------------------------------------------

/// Video ids in the library (for showing filled hearts / "in library" state).
pub async fn liked_ids(State(st): State<SharedState>) -> ApiResult<Vec<String>> {
    Ok(Json(
        st.db
            .call(|c| {
                let mut s = c.prepare("SELECT DISTINCT video_id FROM liked WHERE video_id IS NOT NULL")?;
                let rows = s.query_map([], |r| r.get(0))?;
                rows.collect()
            })
            .await?,
    ))
}

pub async fn add_to_library(st: &SharedState, t: &Track) -> anyhow::Result<()> {
    st.db.upsert_tracks(vec![t.clone()]).await?;
    let t = t.clone();
    st.db
        .call(move |c| {
            c.execute(
                "INSERT INTO liked(provider,provider_id,title,artist,album,duration,video_id,status,added_at)
                 VALUES('local',?1,?2,?3,?4,?5,?1,'matched',?6)
                 ON CONFLICT(provider,provider_id) DO NOTHING",
                params![t.video_id, t.title, t.artist, t.album, t.duration, now()],
            )
            .map(|_| ())
        })
        .await
}

pub async fn remove_from_library(st: &SharedState, video_id: &str) -> anyhow::Result<usize> {
    let v = video_id.to_string();
    st.db.call(move |c| c.execute("DELETE FROM liked WHERE video_id=?1", [v])).await
}

#[derive(Deserialize)]
pub struct AddBody {
    track: Track,
}

pub async fn add(State(st): State<SharedState>, Json(b): Json<AddBody>) -> ApiResult<serde_json::Value> {
    add_to_library(&st, &b.track).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn remove(State(st): State<SharedState>, axum::extract::Path(id): axum::extract::Path<i64>) -> ApiResult<serde_json::Value> {
    st.db.call(move |c| c.execute("DELETE FROM liked WHERE id=?1", [id])).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct MatchBody {
    track: Track,
}

/// Manually set (or change) which YouTube track a library entry plays.
pub async fn set_match(
    State(st): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(b): Json<MatchBody>,
) -> ApiResult<serde_json::Value> {
    st.db.upsert_tracks(vec![b.track.clone()]).await?;
    let vid = b.track.video_id.clone();
    let entry: Option<(String, String)> = st
        .db
        .call(move |c| {
            c.execute("UPDATE liked SET video_id=?1, status='manual' WHERE id=?2", params![vid, id])?;
            c.query_row("SELECT artist, title FROM liked WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(Some)
                .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
        })
        .await?;
    let Some((artist, title)) = entry else { return Err(AppError::NotFound) };
    // Future imports of the same song use this choice too.
    resolver::remember(&st.db, &artist, &title, &b.track.video_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn rematch(State(st): State<SharedState>) -> ApiResult<JobStarted> {
    let job_id = importers::start_rematch(st.clone()).await?;
    Ok(Json(JobStarted { job_id }))
}

pub async fn import_lastfm(State(st): State<SharedState>) -> ApiResult<JobStarted> {
    if st.lastfm.session().await.is_none() {
        return Err(AppError::NotConfigured("Connect Last.fm in Settings first".into()));
    }
    let job_id = importers::start(st.clone(), "lastfm", importers::lastfm_loved(st.clone())).await?;
    Ok(Json(JobStarted { job_id }))
}

/// Two-way sync now: library songs get loved on Last.fm and loved tracks join the library.
pub async fn sync_lastfm(State(st): State<SharedState>) -> ApiResult<JobStarted> {
    if st.lastfm.session().await.is_none() {
        return Err(AppError::NotConfigured("Connect Last.fm in Settings first".into()));
    }
    let job_id = importers::start_lastfm_two_way(st.clone()).await?;
    Ok(Json(JobStarted { job_id }))
}

// ---- the heart: library + Last.fm love ---------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LikedState {
    in_library: bool,
    lastfm_loved: Option<bool>,
    liked: bool,
}

async fn liked_state(st: &SharedState, video_id: &str, check_lastfm: bool) -> anyhow::Result<LikedState> {
    let v = video_id.to_string();
    let in_library: bool = st
        .db
        .call(move |c| c.query_row("SELECT EXISTS(SELECT 1 FROM liked WHERE video_id=?1)", [v], |r| r.get(0)))
        .await?;
    let lastfm_loved = if check_lastfm {
        match st.db.get_track(video_id).await? {
            Some(t) => st.lastfm.is_loved(&t).await.unwrap_or(None),
            None => None,
        }
    } else {
        None
    };
    Ok(LikedState { in_library, liked: in_library || lastfm_loved == Some(true), lastfm_loved })
}

pub async fn get_liked(State(st): State<SharedState>, axum::extract::Path(video_id): axum::extract::Path<String>) -> ApiResult<LikedState> {
    Ok(Json(liked_state(&st, &video_id, true).await?))
}

#[derive(Deserialize)]
pub struct SetLikedBody {
    liked: bool,
    track: Track,
}

/// Heart toggle: adds/removes the track from the library and loves/unloves it on Last.fm
/// (when connected and the "love on Last.fm" preference is on).
pub async fn set_liked(
    State(st): State<SharedState>,
    axum::extract::Path(video_id): axum::extract::Path<String>,
    Json(b): Json<SetLikedBody>,
) -> ApiResult<LikedState> {
    if b.liked {
        add_to_library(&st, &b.track).await?;
    } else {
        remove_from_library(&st, &video_id).await?;
    }
    let lastfm = st.lastfm.session().await.is_some()
        && st.db.get_bool("prefs.love_on_lastfm", true).await
        && st.db.active_profile().await == crate::services::profile::MAIN;
    let mut state = liked_state(&st, &video_id, false).await?;
    if lastfm {
        let r = if b.liked { st.lastfm.love(&b.track).await } else { st.lastfm.unlove(&b.track).await };
        match r {
            Ok(()) => state.lastfm_loved = Some(b.liked),
            Err(e) => tracing::warn!("Last.fm love/unlove failed: {e:#}"),
        }
    }
    state.liked = state.in_library || state.lastfm_loved == Some(true);
    Ok(Json(state))
}
