//! Audio delivery. Plex is preferred when the same track exists in the library; then a recent
//! MP3 copy of a YouTube play (see services::audiocache); otherwise audio comes from YouTube
//! through yt-dlp. Songs that are only in Plex (`plex_<ratingKey>` ids) always come from Plex.

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::{
    error::{ApiResult, AppError},
    routes::music::track_or_fetch,
    services::{
        audiocache, missing,
        plex::{self, PlexTrack},
        spotify,
        ytdlp::AudioLevel,
    },
    state::SharedState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    /// plex | cache | youtube
    source: String,
    plex: Option<PlexTrack>,
    /// Bitrate (kbps) of the audio `/stream` sends for the same `kbps`/`opus` arguments.
    kbps: Option<u32>,
}

/// Quality arguments shared by `/stream` and `/source`.
#[derive(Deserialize, Default, Clone, Copy)]
pub struct QualityQuery {
    /// Bitrate budget in kbps, from the browser's measured connection to this server. None: no limit.
    #[serde(default)]
    kbps: Option<u32>,
    /// `0` when the browser can't play WebM/Opus.
    #[serde(default)]
    opus: Option<u8>,
}

impl QualityQuery {
    fn level(&self) -> AudioLevel {
        AudioLevel::for_budget(self.kbps)
    }
    fn opus(&self) -> bool {
        self.opus != Some(0)
    }
    /// The cached MP3 fits the client's bitrate budget.
    fn cache_ok(&self) -> bool {
        self.kbps.is_none_or(|k| k >= audiocache::KBPS)
    }
}

/// A Plex-only song when Plex can't play it (server unreachable, disconnected, or file gone).
fn plex_only_unavailable() -> AppError {
    AppError::NotConfigured("This song is only in your Plex library, and Plex can't play it right now".into())
}

async fn plex_match(st: &SharedState, video_id: &str) -> Option<PlexTrack> {
    if plex::is_plex_id(video_id) {
        // Only in Plex: play it whether or not "Prefer Plex" is on.
        return match st.plex.track_for_id(video_id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("plex lookup failed for {video_id}: {e:#}");
                None
            }
        };
    }
    if !st.plex.enabled().await {
        return None;
    }
    let track = track_or_fetch(st, video_id).await.ok()?;
    match st.plex.find_match(&track).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("plex match failed for {video_id}: {e:#}");
            None
        }
    }
}

/// The YouTube video behind a song id: itself (or a video picked instead of it), or for a Spotify
/// song the video it's matched to (matched now if it hasn't been yet — the only time a Spotify
/// song costs a YouTube search).
async fn youtube_video(st: &SharedState, video_id: &str) -> Result<String, AppError> {
    if !spotify::is_spotify_id(video_id) {
        let picked = crate::services::resolver::video_override(&st.db, video_id).await?;
        return Ok(picked.unwrap_or_else(|| video_id.to_string()));
    }
    match spotify::youtube_id(st, video_id).await {
        Ok(Some(id)) => Ok(id),
        Ok(None) => {
            missing::mark(st, video_id, "no match on YouTube").await;
            Err(AppError::NotConfigured("Couldn't find this song on YouTube or in Plex. Pick one with ⋯ → Change audio source.".into()))
        }
        Err(e) => Err(AppError::Other(e)),
    }
}

pub async fn source(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Query(q): Query<QualityQuery>,
) -> ApiResult<SourceInfo> {
    spotify::art_on_play(st.clone(), &video_id);
    if let Some(pt) = plex_match(&st, &video_id).await {
        let kbps = st.plex.stream_url(&pt, q.kbps).await.ok().map(|(_, k)| k);
        return Ok(Json(SourceInfo { source: "plex".into(), plex: Some(pt), kbps }));
    }
    if plex::is_plex_id(&video_id) {
        return Err(plex_only_unavailable());
    }
    let youtube = youtube_video(&st, &video_id).await?;
    if st.audio_cache.peek(&youtube, q.cache_ok()) {
        return Ok(Json(SourceInfo { source: "cache".into(), plex: None, kbps: Some(audiocache::KBPS) }));
    }
    // Same (cached) resolve the stream itself uses, so this rarely runs yt-dlp.
    let kbps = match st.ytdlp.resolve(&youtube, q.level(), q.opus()).await {
        Ok(r) => r.kbps.or(Some(q.level().nominal_kbps())),
        Err(e) => {
            if missing::video_gone(&format!("{e:#}")) {
                missing::mark(&st, &video_id, &format!("{e:#}")).await;
            }
            None
        }
    };
    Ok(Json(SourceInfo { source: "youtube".into(), plex: None, kbps }))
}

#[derive(Deserialize)]
pub struct StreamQuery {
    /// Force a source: `youtube` skips the Plex lookup, `pipe` streams yt-dlp's stdout directly.
    #[serde(default)]
    via: Option<String>,
    // Same as QualityQuery (`serde(flatten)` breaks number parsing in query strings).
    #[serde(default)]
    kbps: Option<u32>,
    #[serde(default)]
    opus: Option<u8>,
}

impl StreamQuery {
    fn quality(&self) -> QualityQuery {
        QualityQuery { kbps: self.kbps, opus: self.opus }
    }
}

pub async fn stream(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !video_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || video_id.len() > 40 {
        return Err(AppError::bad("invalid video id"));
    }
    let range = headers.get(header::RANGE).cloned();
    let quality = q.quality();
    // A Spotify song's own album art, the first time it plays (not before, to save requests).
    spotify::art_on_play(st.clone(), &video_id);
    // The song as the app knows it (a Spotify song plays from a YouTube video below).
    let song_id = video_id.clone();

    let plex_only = plex::is_plex_id(&video_id);
    if q.via.is_none() || plex_only {
        if let Some(pt) = plex_match(&st, &video_id).await {
            let (url, kbps) = st.plex.stream_url(&pt, quality.kbps).await?;
            match proxy(&st.http, &url, range.clone(), &[], None, None).await {
                Ok(resp) => {
                    missing::clear(&st, &song_id).await;
                    return Ok(with_kbps(with_source(resp, "plex"), Some(kbps)));
                }
                Err(e) if plex_only => tracing::warn!("plex stream failed for {video_id}: {e:#}"),
                Err(e) => tracing::warn!("plex stream failed, falling back to youtube: {e:#}"),
            }
        }
    }
    if plex_only {
        return Err(plex_only_unavailable());
    }
    let video_id = youtube_video(&st, &video_id).await?;

    if q.via.as_deref() != Some("pipe") {
        let from_start = range_start(range.as_ref()) == 0;
        if let Some(path) = st.audio_cache.choose(&video_id, from_start, quality.cache_ok()) {
            match serve_file(path, range.clone()).await {
                Some(resp) => {
                    missing::clear(&st, &song_id).await;
                    return Ok(with_kbps(with_source(resp, "cache"), Some(audiocache::KBPS)));
                }
                None => tracing::warn!("cached mp3 for {video_id} went missing, streaming from youtube"),
            }
        }

        for attempt in 0..2 {
            let resolved = match st.ytdlp.resolve(&video_id, quality.level(), quality.opus()).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("yt-dlp resolve {video_id}: {e:#}");
                    // The video itself is gone: piping it would fail the same way.
                    if missing::video_gone(&format!("{e:#}")) {
                        missing::mark(&st, &song_id, &format!("{e:#}")).await;
                        return Err(AppError::NotConfigured(
                            "This video isn't available on YouTube any more. Pick another with ⋯ → Change audio source.".into(),
                        ));
                    }
                    break;
                }
            };
            let extra: Vec<(String, String)> = resolved
                .headers
                .iter()
                .filter(|(k, _)| matches!(k.to_ascii_lowercase().as_str(), "user-agent" | "accept-language" | "referer" | "origin"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            // Copy what passes through into the audio cache, at the High level only.
            let tee = (quality.level() == AudioLevel::High).then(|| Tee {
                cache: &st.audio_cache,
                video_id: &video_id,
                meta: audiocache::Meta {
                    title: resolved.title.clone(),
                    artist: resolved.artist.clone(),
                    album: resolved.album.clone(),
                    thumbnail: resolved.thumbnail.clone(),
                },
            });
            match proxy(&st.http, &resolved.url, range.clone(), &extra, Some(&resolved.mime), tee).await {
                Ok(resp) => {
                    missing::clear(&st, &song_id).await;
                    return Ok(with_kbps(with_source(resp, "youtube"), resolved.kbps));
                }
                Err(e) => {
                    tracing::warn!("googlevideo proxy failed (attempt {attempt}): {e:#}");
                    st.ytdlp.invalidate(&video_id).await;
                }
            }
        }
    }

    // Last resort: pipe yt-dlp's output. No seeking, but works when direct URLs are blocked.
    let mut child = match st.ytdlp.pipe(&video_id, quality.level(), quality.opus()).await {
        Ok(c) => c,
        Err(e) => {
            if missing::video_gone(&format!("{e:#}")) {
                missing::mark(&st, &song_id, &format!("{e:#}")).await;
            }
            return Err(e.into());
        }
    };
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let stream = ReaderStream::new(stdout);
    // Keep the child alive for as long as the body is streaming; it is killed on drop.
    let body = Body::from_stream(futures::StreamExt::map(stream, move |chunk| {
        let _keep = &child;
        chunk
    }));
    let mut resp = Response::new(body);
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4"));
    resp.headers_mut().insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    Ok(with_source(resp, "youtube"))
}

/// `x-audio-kbps`: lets the in-page tailnet player (which reads headers) know the bitrate.
fn with_kbps(mut resp: Response, kbps: Option<u32>) -> Response {
    if let Some(v) = kbps.and_then(|k| HeaderValue::from_str(&k.to_string()).ok()) {
        resp.headers_mut().insert("x-audio-kbps", v);
    }
    resp
}

fn with_source(mut resp: Response, source: &'static str) -> Response {
    resp.headers_mut().insert("x-audio-source", HeaderValue::from_static(source));
    // The same URL serves YouTube's file or, once it's cached, the MP3 copy. A browser keeping
    // either would replay without the server seeing the load (so nothing gets cached), and could
    // splice ranges of one file into the other. Plex's file for a track doesn't change.
    let cache = if source == "plex" { "private, max-age=3600" } else { "no-store" };
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    resp
}

/// First byte a `Range` header asks for (0 without one). Suffix ranges (`bytes=-N`) count as mid-file.
fn range_start(range: Option<&HeaderValue>) -> u64 {
    let Some(spec) = range.and_then(|r| r.to_str().ok()).and_then(|r| r.trim().strip_prefix("bytes=")) else {
        return 0;
    };
    spec.split(['-', ',']).next().and_then(|s| s.trim().parse().ok()).unwrap_or(u64::MAX)
}

/// Serve a local file with Range support. None if it can't be read.
async fn serve_file(path: std::path::PathBuf, range: Option<HeaderValue>) -> Option<Response> {
    let mut req = Request::builder().uri("/");
    if let Some(r) = range {
        req = req.header(header::RANGE, r);
    }
    let resp = ServeFile::new(path).oneshot(req.body(Body::empty()).ok()?).await.ok()?;
    (resp.status() != StatusCode::NOT_FOUND).then(|| resp.map(Body::new))
}

/// Where a response's body sits in the whole file: (offset, total length).
fn body_span(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> Option<(u64, u64)> {
    if status == reqwest::StatusCode::PARTIAL_CONTENT {
        // "bytes 0-1023/4096"
        let cr = headers.get("content-range")?.to_str().ok()?.strip_prefix("bytes ")?;
        let (span, total) = cr.split_once('/')?;
        let start = span.split_once('-')?.0.trim().parse().ok()?;
        return Some((start, total.trim().parse().ok()?));
    }
    if status == reqwest::StatusCode::OK {
        return Some((0, headers.get("content-length")?.to_str().ok()?.parse().ok()?));
    }
    None
}

/// Where `proxy` copies a YouTube body to, for the audio cache.
struct Tee<'a> {
    cache: &'a std::sync::Arc<audiocache::AudioCache>,
    video_id: &'a str,
    meta: audiocache::Meta,
}

/// Forward a (possibly ranged) GET, streaming the body back. With `tee`, the body is also copied
/// into the audio cache for that video id.
async fn proxy(
    http: &reqwest::Client,
    url: &str,
    range: Option<HeaderValue>,
    extra: &[(String, String)],
    mime: Option<&str>,
    tee: Option<Tee<'_>>,
) -> anyhow::Result<Response> {
    let mut req = http.get(url);
    if let Some(r) = range {
        req = req.header(header::RANGE, r);
    }
    for (k, v) in extra {
        req = req.header(k.as_str(), v.as_str());
    }
    let upstream = req.send().await?;
    let status = upstream.status();
    if !(status.is_success() || status == StatusCode::RANGE_NOT_SATISFIABLE) {
        anyhow::bail!("upstream returned {status}");
    }

    let mut builder = Response::builder().status(status.as_u16());
    for name in [header::CONTENT_LENGTH, header::CONTENT_RANGE, header::ACCEPT_RANGES, header::LAST_MODIFIED, header::ETAG] {
        if let Some(v) = upstream.headers().get(name.as_str()) {
            builder = builder.header(name, v.as_bytes());
        }
    }
    let ct = mime
        .map(String::from)
        .or_else(|| upstream.headers().get("content-type").and_then(|v| v.to_str().ok()).map(String::from))
        .unwrap_or_else(|| "application/octet-stream".into());
    builder = builder.header(header::CONTENT_TYPE, ct);
    if !upstream.headers().contains_key("accept-ranges") {
        builder = builder.header(header::ACCEPT_RANGES, "bytes");
    }
    let copy = tee.and_then(|t| {
        let (offset, total) = body_span(status, upstream.headers())?;
        t.cache.tee(t.video_id, offset, total, t.meta)
    });
    let body = futures::StreamExt::map(upstream.bytes_stream(), move |chunk| {
        if let (Some(tx), Ok(bytes)) = (&copy, &chunk) {
            let _ = tx.send(bytes.clone());
        }
        chunk
    });
    Ok(builder.body(Body::from_stream(body))?.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_starts() {
        let h = |s: &str| HeaderValue::from_str(s).unwrap();
        assert_eq!(range_start(None), 0);
        assert_eq!(range_start(Some(&h("bytes=0-"))), 0);
        assert_eq!(range_start(Some(&h("bytes=0-1"))), 0);
        assert_eq!(range_start(Some(&h("bytes=12345-"))), 12345);
        assert_eq!(range_start(Some(&h("bytes=-500"))), u64::MAX);
    }

    #[test]
    fn body_spans() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("content-range", "bytes 100-199/4096".parse().unwrap());
        assert_eq!(body_span(reqwest::StatusCode::PARTIAL_CONTENT, &h), Some((100, 4096)));
        h.insert("content-range", "bytes 100-199/*".parse().unwrap());
        assert_eq!(body_span(reqwest::StatusCode::PARTIAL_CONTENT, &h), None);
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("content-length", "777".parse().unwrap());
        assert_eq!(body_span(reqwest::StatusCode::OK, &h), Some((0, 777)));
    }
}
