//! Account connections, preferences and system status.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    error::{ApiResult, AppError},
    models::Track,
    routes::base_url,
    services::{
        audiocache,
        importers,
        playback::{self, PlaybackEvent},
        plex::{self, MatchMode, PlexPin, PlexServer, PlexTrack},
        tailscale::{self, TailscaleInfo},
        ytauth, ytlog,
        ytmusic::{pace_base_ms, ACCOUNT_KEY, DEFAULT_IMPORT_PACE, IMPORT_PACE_KEY, COOKIE_SOURCE_KEY, COOKIE_SYNCED_KEY},
    },
    state::SharedState,
};

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

// ---- status -----------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    ytdlp: Value,
    ytmusic: Value,
    plex: Value,
    lastfm: Value,
    spotify: Value,
    deezer: Value,
    prefs: Prefs,
    audio_cache: audiocache::Stats,
    tailscale: TailscaleInfo,
    public_url: String,
}

pub async fn status(State(st): State<SharedState>, headers: HeaderMap) -> ApiResult<Status> {
    let (ytdlp_path, ytdlp_version) = match st.ytdlp.binary().await {
        Ok(p) => (Some(p.display().to_string()), st.ytdlp.version().await.ok()),
        Err(_) => (None, None),
    };
    let plex = st.plex.server().await;
    let lastfm_session = st.lastfm.session().await;

    // Host-mode tailscale status is cheap enough to refresh on demand.
    let ts = {
        let cur = st.tailscale.lock().await.clone();
        if cur.mode == "embedded" {
            cur
        } else {
            let fresh = tailscale::host_status(st.cfg.bind).await;
            *st.tailscale.lock().await = fresh.clone();
            fresh
        }
    };

    Ok(Json(Status {
        ytdlp: json!({ "path": ytdlp_path, "version": ytdlp_version }),
        ytmusic: json!({
            "authenticated": st.yt.is_authenticated().await,
            "activeNow": st.yt.signed_in_now().await,
            "source": st.db.get_setting(COOKIE_SOURCE_KEY).await?,
            "account": st.db.get_setting(ACCOUNT_KEY).await?,
            "syncedAt": st.db.get_setting(COOKIE_SYNCED_KEY).await?.and_then(|s| s.parse::<i64>().ok()),
            "syncError": st.db.get_setting(ytauth::SYNC_ERROR_KEY).await?,
            // YouTube answers the saved sign-in as signed out (it expired) since this time.
            "expiredSince": st.yt.session_expired_since().await,
        }),
        plex: json!({
            "connected": plex.is_some(),
            "url": plex.map(|(u, _)| u),
            "loggedIn": st.db.get_setting("plex.account_token").await?.is_some(),
        }),
        lastfm: json!({
            "configured": st.lastfm.keys().await.is_some(),
            "keysFromEnv": st.cfg.lastfm_api_key.is_some(),
            "connected": lastfm_session.is_some(),
            "username": lastfm_session.map(|(_, u)| u),
        }),
        spotify: json!({
            // Where Exportify downloads are picked up from, and whether that was set by hand.
            "exportDirs": crate::services::spotify::export_dirs(&st).await.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(),
            "exportDirSet": st.db.get_setting(crate::services::spotify::EXPORT_DIR_KEY).await?.is_some(),
        }),
        deezer: json!({ "userId": st.db.get_setting("deezer.user_id").await? }),
        prefs: load_prefs(&st).await,
        audio_cache: st.audio_cache.stats().await,
        tailscale: ts,
        public_url: base_url(&st, &headers).await,
    }))
}

pub async fn update_ytdlp(State(st): State<SharedState>) -> ApiResult<Value> {
    let out = st.ytdlp.update().await?;
    Ok(Json(json!({ "output": out, "version": st.ytdlp.version().await.ok() })))
}

// ---- preferences ----------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prefs {
    prefer_plex: bool,
    scrobble: bool,
    report_youtube: bool,
    #[serde(default = "yes")]
    love_on_lastfm: bool,
    /// Daily Last.fm loves sync: "off" | "pull" (loves → library) | "both" (library ↔ loves)
    #[serde(default = "off")]
    lastfm_sync: String,
    #[serde(default)]
    log_youtube: bool,
    /// Spacing of import/sync requests to YouTube: "normal" | "slow" | "very_slow"
    #[serde(default = "default_pace")]
    import_pace: String,
    /// Hours MP3 copies of YouTube plays are kept and used (0: off, -1: forever).
    #[serde(default = "default_cache_hours")]
    audio_cache_hours: i32,
}

fn default_cache_hours() -> i32 {
    audiocache::DEFAULT_HOURS
}

fn default_pace() -> String {
    DEFAULT_IMPORT_PACE.into()
}

pub async fn import_pace(st: &SharedState) -> String {
    st.db
        .get_setting(IMPORT_PACE_KEY)
        .await
        .ok()
        .flatten()
        .filter(|p| pace_base_ms(p).is_some())
        .unwrap_or_else(default_pace)
}

fn off() -> String {
    "off".into()
}

async fn load_prefs(st: &SharedState) -> Prefs {
    Prefs {
        prefer_plex: st.db.get_bool("prefs.prefer_plex", true).await,
        scrobble: st.db.get_bool("prefs.scrobble", true).await,
        report_youtube: st.db.get_bool("prefs.report_youtube", true).await,
        love_on_lastfm: st.db.get_bool("prefs.love_on_lastfm", true).await,
        lastfm_sync: importers::lastfm_sync_mode(&st.db).await,
        log_youtube: ytlog::enabled(),
        import_pace: import_pace(st).await,
        audio_cache_hours: st.audio_cache.hours(),
    }
}

pub async fn set_prefs(State(st): State<SharedState>, Json(p): Json<Prefs>) -> ApiResult<Prefs> {
    if pace_base_ms(&p.import_pace).is_none() {
        return Err(AppError::bad("importPace must be original, normal, slow or very_slow"));
    }
    if p.audio_cache_hours != audiocache::FOREVER && !(0..=audiocache::MAX_HOURS).contains(&p.audio_cache_hours) {
        return Err(AppError::bad(format!(
            "audioCacheHours must be 0 to {}, or {} to keep copies forever",
            audiocache::MAX_HOURS,
            audiocache::FOREVER
        )));
    }
    st.db.set_setting("prefs.prefer_plex", if p.prefer_plex { "1" } else { "0" }).await?;
    st.db.set_setting("prefs.scrobble", if p.scrobble { "1" } else { "0" }).await?;
    st.db.set_setting("prefs.report_youtube", if p.report_youtube { "1" } else { "0" }).await?;
    st.db.set_setting("prefs.love_on_lastfm", if p.love_on_lastfm { "1" } else { "0" }).await?;
    let mode = match p.lastfm_sync.as_str() {
        m @ ("off" | "pull" | "both") => m,
        _ => return Err(AppError::bad("lastfmSync must be off, pull or both")),
    };
    st.db.set_setting(importers::LASTFM_SYNC_MODE, mode).await?;
    st.db.set_setting(ytlog::PREF, if p.log_youtube { "1" } else { "0" }).await?;
    ytlog::set_enabled(p.log_youtube);
    st.db.set_setting(IMPORT_PACE_KEY, &p.import_pace).await?;
    st.yt.set_pace(&p.import_pace);
    st.audio_cache.set_hours(p.audio_cache_hours).await?;
    Ok(Json(load_prefs(&st).await))
}

// ---- YouTube Music ----------------------------------------------------------------

#[derive(Deserialize)]
pub struct CookieBody {
    cookie: String,
}

pub async fn set_yt_cookie(State(st): State<SharedState>, Json(b): Json<CookieBody>) -> ApiResult<Value> {
    // Accept either the raw header value or a copied "cookie: ..." line.
    let cookie = b.cookie.trim().trim_start_matches("Cookie:").trim_start_matches("cookie:").trim().to_string();
    if !cookie.contains("SAPISID") {
        return Err(AppError::bad("Cookie must contain SAPISID / __Secure-3PAPISID (copy it from a logged-in music.youtube.com request)"));
    }
    // Only keep it if YouTube actually signs us in with it.
    let account = ytauth::verify(&st, &cookie).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    ytauth::save(&st, &cookie, "manual").await?;
    st.db.set_setting(ACCOUNT_KEY, &account).await?;
    Ok(Json(json!({ "ok": true, "account": account })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportBrowserBody {
    /// Specific browser (`firefox`, `chrome`, …); omitted = try them all.
    #[serde(default)]
    browser: Option<String>,
    #[serde(default = "yes")]
    keep_in_sync: bool,
}

fn yes() -> bool {
    true
}

/// Sign in by importing the YouTube session from a browser on the server machine.
pub async fn import_yt_browser(State(st): State<SharedState>, Json(b): Json<ImportBrowserBody>) -> ApiResult<Value> {
    let browser = b.browser.filter(|s| !s.is_empty());
    let (found, account) = ytauth::import(&st, browser.as_deref(), b.keep_in_sync)
        .await
        .map_err(|e| AppError::bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true, "browser": found, "account": account })))
}

pub async fn sync_yt_browser(State(st): State<SharedState>) -> ApiResult<Value> {
    let synced = ytauth::sync_once(&st).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true, "synced": synced })))
}

pub async fn delete_yt_cookie(State(st): State<SharedState>) -> ApiResult<Value> {
    st.db.delete_settings("ytmusic.cookie").await?; // cookie, source, sync time, account
    st.yt.reset_session_state().await;
    Ok(ok())
}

// ---- Last.fm --------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastfmKeys {
    api_key: String,
    api_secret: String,
}

pub async fn set_lastfm_keys(State(st): State<SharedState>, Json(b): Json<LastfmKeys>) -> ApiResult<Value> {
    st.db.set_setting("lastfm.api_key", b.api_key.trim()).await?;
    st.db.set_setting("lastfm.api_secret", b.api_secret.trim()).await?;
    Ok(ok())
}

#[derive(Serialize)]
pub struct UrlResponse {
    url: String,
}

#[derive(Deserialize)]
pub struct LastfmLoginQuery {
    callback: String,
}

/// Last.fm sends the user back to `callback?token=...` (a page route in the web app).
pub async fn lastfm_login(State(st): State<SharedState>, Query(q): Query<LastfmLoginQuery>) -> ApiResult<UrlResponse> {
    Ok(Json(UrlResponse { url: st.lastfm.auth_url(&q.callback).await.map_err(|e| AppError::NotConfigured(e.to_string()))? }))
}

#[derive(Deserialize)]
pub struct LastfmSessionBody {
    token: String,
}

pub async fn lastfm_session(State(st): State<SharedState>, Json(b): Json<LastfmSessionBody>) -> ApiResult<Value> {
    let user = st.lastfm.complete_auth(&b.token).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true, "username": user })))
}

pub async fn lastfm_disconnect(State(st): State<SharedState>) -> ApiResult<Value> {
    st.lastfm.disconnect().await?;
    Ok(ok())
}

// ---- Plex -----------------------------------------------------------------------

pub async fn plex_pin(State(st): State<SharedState>) -> ApiResult<PlexPin> {
    Ok(Json(st.plex.start_pin().await?))
}

pub async fn plex_pin_check(State(st): State<SharedState>, Path(id): Path<i64>) -> ApiResult<Value> {
    Ok(Json(json!({ "authorized": st.plex.check_pin(id).await? })))
}

pub async fn plex_servers(State(st): State<SharedState>) -> ApiResult<Vec<PlexServer>> {
    Ok(Json(st.plex.servers().await?))
}

#[derive(Deserialize)]
pub struct PlexServerBody {
    url: String,
    token: String,
}

pub async fn plex_set_server(State(st): State<SharedState>, Json(b): Json<PlexServerBody>) -> ApiResult<Value> {
    let id = st.plex.set_server(&b.url, &b.token).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true, "machineIdentifier": id })))
}

pub async fn plex_disconnect(State(st): State<SharedState>) -> ApiResult<Value> {
    st.plex.disconnect().await?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct PlexSearchQuery {
    q: String,
}

/// Tracks on the Plex server, as songs with Plex-only ids (for picking a library match).
pub async fn plex_search(State(st): State<SharedState>, Query(q): Query<PlexSearchQuery>) -> ApiResult<Vec<Track>> {
    if st.plex.server().await.is_none() {
        return Err(AppError::NotConfigured("Connect Plex in Settings first".into()));
    }
    let hits = st.plex.search(q.q.trim()).await.map_err(|e| AppError::bad(format!("Plex search failed: {e:#}")))?;
    Ok(Json(hits.iter().map(|h| h.to_track()).collect()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexMatchBody {
    rating_key: String,
}

/// Play a YouTube-matched song from a Plex track of your choosing.
pub async fn plex_set_match(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Json(b): Json<PlexMatchBody>,
) -> ApiResult<PlexTrack> {
    if plex::is_plex_id(&video_id) {
        return Err(AppError::bad("that song is already a Plex track"));
    }
    let t = st.plex.pin(&video_id, &b.rating_key).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    crate::services::missing::clear(&st, &video_id).await;
    Ok(Json(t))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexCandidate {
    /// The Plex track, under its Plex-only id.
    track: Track,
    score: f32,
    live: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexChoice {
    mode: plex::MatchMode,
    /// The rating key it plays from now (in automatic mode, also one not yet stored).
    current: Option<String>,
    /// Possible Plex copies, best first.
    candidates: Vec<PlexCandidate>,
}

/// Which Plex track a song plays from, and the alternatives to choose from.
pub async fn plex_get_match(State(st): State<SharedState>, Path(video_id): Path<String>) -> ApiResult<PlexChoice> {
    if st.plex.server().await.is_none() {
        return Err(AppError::NotConfigured("Connect Plex in Settings first".into()));
    }
    if plex::is_plex_id(&video_id) {
        return Err(AppError::bad("that song is a Plex track"));
    }
    let track = crate::routes::music::track_or_fetch(&st, &video_id).await?;
    let (mode, stored) = st.plex.choice(&video_id).await?;
    let found = st.plex.candidates(&track).await.map_err(|e| AppError::bad(format!("Plex search failed: {e:#}")))?;
    let current = match (mode, stored) {
        (MatchMode::Off, _) => None,
        (_, Some(t)) => Some(t.rating_key),
        // Not matched yet (or due a redo): what automatic matching would pick.
        (_, None) => found.iter().find(|(_, s)| *s >= plex::MATCH_THRESHOLD).map(|(h, _)| h.track.rating_key.clone()),
    };
    let candidates = found
        .into_iter()
        .filter(|(_, s)| *s >= 0.5)
        .take(30)
        .map(|(h, score)| PlexCandidate {
            live: crate::services::matching::looks_live(&h.track.title, h.album.as_deref()),
            track: h.to_track(),
            score,
        })
        .collect();
    Ok(Json(PlexChoice { mode, current, candidates }))
}

#[derive(Deserialize)]
pub struct PlexClearQuery {
    /// `off`: never play this song from Plex; `auto` (default): back to automatic matching.
    #[serde(default)]
    mode: Option<String>,
}

pub async fn plex_clear_match(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Query(q): Query<PlexClearQuery>,
) -> ApiResult<Value> {
    if plex::is_plex_id(&video_id) {
        return Err(AppError::bad("that song is a Plex track"));
    }
    match q.mode.as_deref() {
        Some("off") => st.plex.block(&video_id).await?,
        None | Some("auto") => st.plex.forget(&video_id).await?,
        Some(m) => return Err(AppError::bad(format!("unknown mode {m}"))),
    }
    Ok(ok())
}

#[derive(Deserialize)]
pub struct PlexArtQuery {
    path: String,
}

/// Artwork of Plex tracks (the server needs its token, which browsers don't get).
pub async fn plex_art(State(st): State<SharedState>, Query(q): Query<PlexArtQuery>) -> Result<Response, AppError> {
    let bytes = st.plex.art(&q.path).await.map_err(|e| AppError::bad(format!("{e:#}")))?;
    let mut resp = crate::routes::art::image_response(bytes);
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    Ok(resp)
}

// ---- YouTube request log ------------------------------------------------------------

pub async fn youtube_log(State(st): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(json!({
        "enabled": ytlog::enabled(),
        "backgroundGapMs": st.yt.pace_gap_ms(),
        "path": ytlog::path().map(|p| p.display().to_string()),
        "stats": ytlog::stats(),
        "recent": ytlog::recent(100),
    })))
}

pub async fn youtube_log_file() -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    let path = ytlog::path().ok_or(AppError::NotFound)?;
    let mut text = tokio::fs::read_to_string(path.with_extension("log.1")).await.unwrap_or_default();
    text += &tokio::fs::read_to_string(&path).await.unwrap_or_default();
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"youtube.log\""),
        ],
        text,
    )
        .into_response())
}

pub async fn clear_youtube_log() -> ApiResult<Value> {
    ytlog::clear();
    Ok(ok())
}

// ---- playback reports ---------------------------------------------------------------

pub async fn playback_event(State(st): State<SharedState>, Json(ev): Json<PlaybackEvent>) -> ApiResult<Value> {
    playback::handle(&st, ev).await?;
    Ok(ok())
}
