//! Jellyfin settings and matching endpoints.
//!
//! - `PUT /jellyfin/server {url, username, password}` → `JellyfinInfo`
//! - `DELETE /jellyfin` → `{ok}`
//! - `GET /jellyfin/search?q=` → `Track[]` (ids `jf_<itemId>`)
//! - `GET /jellyfin/matches/{video_id}` → `sources::Choice`
//! - `PUT /jellyfin/matches/{video_id} {itemId}` → `JellyfinTrack`
//! - `DELETE /jellyfin/matches/{video_id}?mode=off|auto` → `{ok}`
//! - `GET /jellyfin/art?id=<itemId>` → image

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue},
    response::Response,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    error::{ApiResult, AppError},
    models::Track,
    services::{
        jellyfin::{self, JellyfinInfo, JellyfinTrack},
        matching,
        plex::MatchMode,
        sources::{Candidate, Choice},
    },
    state::SharedState,
};

fn not_connected() -> AppError {
    AppError::NotConfigured("Connect Jellyfin in Settings first".into())
}

async fn require_connected(st: &SharedState) -> Result<(), AppError> {
    if st.jellyfin.configured().await { Ok(()) } else { Err(not_connected()) }
}

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct ConnectBody {
    pub url: String,
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// Sign in to a Jellyfin server.
pub async fn connect(State(st): State<SharedState>, Json(b): Json<ConnectBody>) -> ApiResult<JellyfinInfo> {
    let info = st.jellyfin.connect(&b.url, &b.username, &b.password).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    Ok(Json(info))
}

pub async fn disconnect(State(st): State<SharedState>) -> ApiResult<Value> {
    st.jellyfin.disconnect().await?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
}

/// Tracks on the Jellyfin server, as songs with Jellyfin-only ids (for picking a library match).
pub async fn search(State(st): State<SharedState>, Query(q): Query<SearchQuery>) -> ApiResult<Vec<Track>> {
    require_connected(&st).await?;
    let hits = st.jellyfin.search(q.q.trim()).await.map_err(|e| AppError::bad(format!("Jellyfin search failed: {e:#}")))?;
    Ok(Json(hits.iter().map(|h| h.to_track()).collect()))
}

/// Which Jellyfin track a song plays from, and the alternatives to choose from.
pub async fn get_match(State(st): State<SharedState>, Path(video_id): Path<String>) -> ApiResult<Choice> {
    require_connected(&st).await?;
    if jellyfin::is_jellyfin_id(&video_id) {
        return Err(AppError::bad("that song is a Jellyfin track"));
    }
    let track = crate::routes::music::track_or_fetch(&st, &video_id).await?;
    let (mode, stored) = st.jellyfin.choice(&video_id).await?;
    let found = st.jellyfin.candidates(&track).await.map_err(|e| AppError::bad(format!("Jellyfin search failed: {e:#}")))?;
    let current = match (mode, stored) {
        (MatchMode::Off, _) => None,
        (_, Some(t)) => Some(t.item_id),
        // Not matched yet (or due a redo): what automatic matching would pick.
        (_, None) => found.iter().find(|(_, s)| *s >= jellyfin::MATCH_THRESHOLD).map(|(h, _)| h.track.item_id.clone()),
    };
    let candidates = found
        .into_iter()
        .filter(|(_, s)| *s >= 0.5)
        .take(30)
        .map(|(h, score)| Candidate { live: matching::looks_live(&h.track.title, h.album.as_deref()), track: h.to_track(), score })
        .collect();
    Ok(Json(Choice { mode, current, candidates }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchBody {
    pub item_id: String,
}

/// Play a song from a Jellyfin track of your choosing.
pub async fn set_match(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Json(b): Json<MatchBody>,
) -> ApiResult<JellyfinTrack> {
    require_connected(&st).await?;
    if jellyfin::is_jellyfin_id(&video_id) {
        return Err(AppError::bad("that song is already a Jellyfin track"));
    }
    // The item can be given as its id or as a `jf_` song id.
    let item = b.item_id.trim();
    let item = item.strip_prefix(jellyfin::ID_PREFIX).unwrap_or(item);
    if jellyfin::normalize_item_id(item).is_none() {
        return Err(AppError::bad("not a Jellyfin item id"));
    }
    let t = st.jellyfin.pin(&video_id, item).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    crate::services::missing::clear(&st, &video_id).await;
    Ok(Json(t))
}

#[derive(Deserialize)]
pub struct ClearQuery {
    /// `off`: never play this song from Jellyfin; `auto` (default): back to automatic matching.
    #[serde(default)]
    pub mode: Option<String>,
}

pub async fn clear_match(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Query(q): Query<ClearQuery>,
) -> ApiResult<Value> {
    if jellyfin::is_jellyfin_id(&video_id) {
        return Err(AppError::bad("that song is a Jellyfin track"));
    }
    match q.mode.as_deref() {
        Some("off") => st.jellyfin.block(&video_id).await?,
        None | Some("auto") => st.jellyfin.forget(&video_id).await?,
        Some(m) => return Err(AppError::bad(format!("unknown mode {m}"))),
    }
    Ok(ok())
}

#[derive(Deserialize)]
pub struct ArtQuery {
    pub id: String,
}

/// Artwork of Jellyfin tracks (the server needs its token, which browsers don't get).
pub async fn art(State(st): State<SharedState>, Query(q): Query<ArtQuery>) -> Result<Response, AppError> {
    if jellyfin::normalize_item_id(&q.id).is_none() {
        return Err(AppError::bad("not a Jellyfin item id"));
    }
    require_connected(&st).await?;
    let bytes = st.jellyfin.art(&q.id).await.map_err(|e| {
        tracing::debug!("jellyfin art {}: {e:#}", q.id);
        AppError::NotFound
    })?;
    let mut resp = crate::routes::art::image_response(bytes);
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    Ok(resp)
}
