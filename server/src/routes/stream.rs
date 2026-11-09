//! Audio delivery. Sources are tried in the order chosen in Settings → Audio sources: Plex,
//! Jellyfin and the local music folder when they have the song (matched by its tags), and
//! YouTube (a recent MP3 copy of a YouTube play, see services::audiocache, else yt-dlp). When one
//! can't play the song, the next is tried. A song that's only in one library (`plex_…`, `jf_…`,
//! `local_…`) tries that library first, then the other libraries; never YouTube.

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
    models::Track,
    routes::music::track_or_fetch,
    services::{
        audiocache, missing,
        plex::PlexTrack,
        resolver,
        sources::{self, ItemInfo, LibraryItem, Source},
        spotify,
        ytdlp::AudioLevel,
    },
    state::SharedState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    /// plex | jellyfin | local | cache | youtube
    source: String,
    plex: Option<PlexTrack>,
    /// Bitrate (kbps) of the audio `/stream` sends for the same `kbps`/`opus` arguments.
    kbps: Option<u32>,
    /// The library file it plays from (Plex or Jellyfin).
    item: Option<ItemInfo>,
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

/// `/source` arguments: quality, and optionally which sources to consider.
#[derive(Deserialize, Default)]
pub struct SourceQuery {
    #[serde(default)]
    kbps: Option<u32>,
    #[serde(default)]
    opus: Option<u8>,
    /// Comma-separated sources (`plex,youtube`): only those, in the usual order. The web app asks
    /// this way about the sources ahead of its music folder before playing a song from the folder;
    /// a song none of them has isn't marked missing then (the folder may have it).
    #[serde(default)]
    only: Option<String>,
}

impl SourceQuery {
    fn quality(&self) -> QualityQuery {
        QualityQuery { kbps: self.kbps, opus: self.opus }
    }

    fn only(&self) -> Option<Vec<Source>> {
        let list = self.only.as_deref()?;
        Some(list.split(',').filter_map(|s| serde_json::from_value(serde_json::Value::String(s.trim().to_string())).ok()).collect())
    }
}

/// What one source did with a song.
enum Tried<T> {
    Played(T),
    /// It doesn't have the song.
    NotThere,
    /// It has (or may have) the song but couldn't play it now.
    Failed(AppError),
}

/// A song with its sources: the order to try them in, and its tags for matching it in the
/// libraries (looked up once, only when a library needs them).
struct Song<'a> {
    st: &'a SharedState,
    id: &'a str,
    plan: Vec<Source>,
    track: Option<Track>,
}

impl<'a> Song<'a> {
    async fn new(st: &'a SharedState, id: &'a str) -> Song<'a> {
        let plan = sources::play_plan(st, id).await;
        let own = sources::own_source(id);
        let needs_tags = plan.iter().any(|s| s.is_library() && Some(*s) != own);
        let track = if needs_tags { track_or_fetch(st, id).await.ok() } else { None };
        Song { st, id, plan, track }
    }

    /// The song's copy in library `s`.
    async fn item(&self, s: Source) -> Tried<LibraryItem> {
        match sources::find_item(self.st, s, self.id, self.track.as_ref()).await {
            Ok(Some(item)) => Tried::Played(item),
            Ok(None) => Tried::NotThere,
            Err(e) => {
                tracing::warn!("{} lookup failed for {}: {e:#}", s.as_str(), self.id);
                Tried::Failed(self.unavailable(s))
            }
        }
    }

    /// Library `s` has the song but can't play it now (server unreachable, file gone…).
    fn unavailable(&self, s: Source) -> AppError {
        let msg = if sources::own_source(self.id) == Some(s) {
            format!("This song is only in {}, which can't play it right now", s.label())
        } else {
            format!("{} can't play this song right now", capitalized(s.label()))
        };
        AppError::NotConfigured(msg)
    }

    /// Nothing played the song: the last failure, or (when every source said it doesn't have the
    /// song) it's marked as missing.
    async fn nothing_plays(&self, failure: Option<AppError>) -> AppError {
        if let Some(e) = failure {
            return e;
        }
        let msg = sources::not_found_message(&self.plan);
        if self.plan.is_empty() {
            // Every source is off: nothing was looked at, so the song isn't known to be missing.
            return AppError::NotConfigured(msg);
        }
        missing::mark(self.st, self.id, &msg).await;
        AppError::NotConfigured(format!("{msg}. Pick a copy with ⋯ → Change audio source."))
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The YouTube video behind a song id: itself (or a video picked instead of it), or for a Spotify
/// or imported song the video it's matched to (matched now if it hasn't been yet — the only time
/// such a song costs a YouTube search). None: YouTube has nothing convincing.
async fn youtube_video(st: &SharedState, video_id: &str) -> Result<Option<String>, AppError> {
    if !resolver::is_deferred_id(video_id) {
        let picked = resolver::video_override(&st.db, video_id).await?;
        return Ok(Some(picked.unwrap_or_else(|| video_id.to_string())));
    }
    Ok(resolver::deferred_youtube_id(st, video_id).await?)
}

pub async fn source(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Query(sq): Query<SourceQuery>,
) -> ApiResult<SourceInfo> {
    let q = sq.quality();
    let only = sq.only();
    if only.is_none() {
        spotify::art_on_play(st.clone(), &video_id);
    }
    let mut song = Song::new(&st, &video_id).await;
    if let Some(only) = &only {
        song.plan.retain(|s| only.contains(s));
    }
    let mut failure = None;
    for &s in &song.plan {
        let tried = match s {
            Source::Youtube => youtube_source(&st, &video_id, q).await,
            lib => match song.item(lib).await {
                Tried::Played(item) => describe(&song, item, q).await,
                Tried::NotThere => Tried::NotThere,
                Tried::Failed(e) => Tried::Failed(e),
            },
        };
        match tried {
            Tried::Played(info) => return Ok(Json(info)),
            Tried::NotThere => {}
            Tried::Failed(e) => failure = Some(e),
        }
    }
    if only.is_some() {
        // Asked about some sources only: none of them plays it, which says nothing about the rest.
        return Err(failure.unwrap_or(AppError::NotFound));
    }
    Err(song.nothing_plays(failure).await)
}

/// What `/stream` would send from a library file.
async fn describe(song: &Song<'_>, item: LibraryItem, q: QualityQuery) -> Tried<SourceInfo> {
    let st = song.st;
    let info = |kbps, plex| SourceInfo { source: item.source().as_str().into(), plex, kbps, item: Some(item.info()) };
    match &item {
        LibraryItem::Plex(t) => {
            let kbps = st.plex.stream_url(t, q.kbps).await.ok().map(|(_, k)| k);
            Tried::Played(info(kbps, Some(t.clone())))
        }
        LibraryItem::Jellyfin(t) => match st.jellyfin.stream_url(t, q.kbps).await {
            Ok((_, kbps)) => Tried::Played(info(Some(kbps), None)),
            Err(e) => {
                tracing::warn!("jellyfin stream url for {}: {e:#}", song.id);
                Tried::Failed(song.unavailable(Source::Jellyfin))
            }
        },
    }
}

/// What `/stream` would send from YouTube: the cached MP3, else what yt-dlp finds.
async fn youtube_source(st: &SharedState, video_id: &str, q: QualityQuery) -> Tried<SourceInfo> {
    let youtube = match youtube_video(st, video_id).await {
        Ok(Some(v)) => v,
        Ok(None) => return Tried::NotThere,
        Err(e) => return Tried::Failed(e),
    };
    if st.audio_cache.peek(&youtube, q.cache_ok()) {
        return Tried::Played(SourceInfo { source: "cache".into(), plex: None, kbps: Some(audiocache::KBPS), item: None });
    }
    // Same (cached) resolve the stream itself uses, so this rarely runs yt-dlp.
    let kbps = match st.ytdlp.resolve(&youtube, q.level(), q.opus()).await {
        Ok(r) => r.kbps.or(Some(q.level().nominal_kbps())),
        Err(e) => {
            if missing::video_gone(&format!("{e:#}")) {
                missing::mark(st, video_id, &format!("{e:#}")).await;
            }
            None
        }
    };
    Tried::Played(SourceInfo { source: "youtube".into(), plex: None, kbps, item: None })
}

#[derive(Deserialize)]
pub struct StreamQuery {
    /// Force a source: `youtube` goes straight to YouTube (when it's on), `pipe` streams
    /// yt-dlp's stdout directly.
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
    // YouTube ids, `spotify_…`, `song_…`, `plex_…`, `jf_<32 hex>`, `local_<n>`.
    if !video_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || video_id.len() > 40 {
        return Err(AppError::bad("invalid video id"));
    }
    let range = headers.get(header::RANGE).cloned();
    let quality = q.quality();
    // A Spotify song's own album art, the first time it plays (not before, to save requests).
    spotify::art_on_play(st.clone(), &video_id);

    let mut song = Song::new(&st, &video_id).await;
    if q.via.is_some() && song.plan.contains(&Source::Youtube) {
        song.plan = vec![Source::Youtube];
    }
    let mut failure = None;
    for &s in &song.plan {
        let tried = match s {
            Source::Youtube => youtube_stream(&st, &video_id, range.clone(), quality, q.via.as_deref() == Some("pipe")).await,
            lib => match song.item(lib).await {
                Tried::Played(item) => library_stream(&song, item, range.clone(), quality).await,
                Tried::NotThere => Tried::NotThere,
                Tried::Failed(e) => Tried::Failed(e),
            },
        };
        match tried {
            Tried::Played(resp) => {
                missing::clear(&st, &video_id).await;
                return Ok(resp);
            }
            Tried::NotThere => {}
            Tried::Failed(e) => {
                tracing::warn!("{} couldn't play {video_id}: {e}", s.as_str());
                failure = Some(e);
            }
        }
    }
    Err(song.nothing_plays(failure).await)
}

/// Serve a song's library file: Plex and Jellyfin proxied (transcoded by them when needed), a
/// local file directly (with Range).
async fn library_stream(song: &Song<'_>, item: LibraryItem, range: Option<HeaderValue>, quality: QualityQuery) -> Tried<Response> {
    let st = song.st;
    let source = item.source();
    let served: anyhow::Result<Option<(Response, Option<u32>)>> = async {
        Ok(match &item {
            LibraryItem::Plex(t) => {
                let (url, kbps) = st.plex.stream_url(t, quality.kbps).await?;
                Some((proxy(&st.http, &url, range, &[], None, None).await?, Some(kbps)))
            }
            LibraryItem::Jellyfin(t) => {
                let (url, kbps) = st.jellyfin.stream_url(t, quality.kbps).await?;
                Some((proxy(&st.http, &url, range, &[], None, None).await?, Some(kbps)))
            }
        })
    }
    .await;
    match served {
        Ok(Some((resp, kbps))) => Tried::Played(with_kbps(with_source(resp, source.as_str()), kbps)),
        // The file went missing from disk (or the index).
        Ok(None) => Tried::NotThere,
        Err(e) => {
            tracing::warn!("{} stream failed for {}: {e:#}", source.as_str(), song.id);
            Tried::Failed(song.unavailable(source))
        }
    }
}

/// Stream a song from YouTube: the cached MP3 when there is one, else the audio yt-dlp finds
/// (proxied, and copied into the cache), else yt-dlp's own output as a last resort.
async fn youtube_stream(
    st: &SharedState,
    song_id: &str,
    range: Option<HeaderValue>,
    quality: QualityQuery,
    pipe_only: bool,
) -> Tried<Response> {
    let video_id = match youtube_video(st, song_id).await {
        Ok(Some(v)) => v,
        Ok(None) => return Tried::NotThere,
        Err(e) => return Tried::Failed(e),
    };
    let video_id = video_id.as_str();

    if !pipe_only {
        let from_start = range_start(range.as_ref()) == 0;
        if let Some(path) = st.audio_cache.choose(video_id, from_start, quality.cache_ok()) {
            match serve_file(path, range.clone()).await {
                Some(resp) => return Tried::Played(with_kbps(with_source(resp, "cache"), Some(audiocache::KBPS))),
                None => tracing::warn!("cached mp3 for {video_id} went missing, streaming from youtube"),
            }
        }

        for attempt in 0..2 {
            let resolved = match st.ytdlp.resolve(video_id, quality.level(), quality.opus()).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("yt-dlp resolve {video_id}: {e:#}");
                    // The video itself is gone: piping it would fail the same way.
                    if missing::video_gone(&format!("{e:#}")) {
                        missing::mark(st, song_id, &format!("{e:#}")).await;
                        return Tried::Failed(AppError::NotConfigured(
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
                video_id,
                meta: audiocache::Meta {
                    title: resolved.title.clone(),
                    artist: resolved.artist.clone(),
                    album: resolved.album.clone(),
                    thumbnail: resolved.thumbnail.clone(),
                },
            });
            match proxy(&st.http, &resolved.url, range.clone(), &extra, Some(&resolved.mime), tee).await {
                Ok(resp) => return Tried::Played(with_kbps(with_source(resp, "youtube"), resolved.kbps)),
                Err(e) => {
                    tracing::warn!("googlevideo proxy failed (attempt {attempt}): {e:#}");
                    st.ytdlp.invalidate(video_id).await;
                }
            }
        }
    }

    // Last resort: pipe yt-dlp's output. No seeking, but works when direct URLs are blocked.
    let mut child = match st.ytdlp.pipe(video_id, quality.level(), quality.opus()).await {
        Ok(c) => c,
        Err(e) => {
            if missing::video_gone(&format!("{e:#}")) {
                missing::mark(st, song_id, &format!("{e:#}")).await;
            }
            return Tried::Failed(e.into());
        }
    };
    let Some(stdout) = child.stdout.take() else { return Tried::Failed(anyhow::anyhow!("no stdout").into()) };
    let stream = ReaderStream::new(stdout);
    // Keep the child alive for as long as the body is streaming; it is killed on drop.
    let body = Body::from_stream(futures::StreamExt::map(stream, move |chunk| {
        let _keep = &child;
        chunk
    }));
    let mut resp = Response::new(body);
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4"));
    resp.headers_mut().insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    Tried::Played(with_source(resp, "youtube"))
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
    // splice ranges of one file into the other. A library's file for a song doesn't change.
    let library = matches!(source, "plex" | "jellyfin" | "local");
    let cache = if library { "private, max-age=3600" } else { "no-store" };
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
