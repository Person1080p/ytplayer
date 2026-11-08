//! Minimal YouTube Music (InnerTube `WEB_REMIX`) client.
//!
//! There is no official API, so this talks to the same private endpoints the
//! music.youtube.com web client uses. Responses are parsed defensively by
//! walking the JSON for known renderer types rather than hard-coding paths,
//! which keeps it working across most of YouTube's frequent layout shuffles.
//!
//! Authentication (needed for personalised home, liked songs and play history)
//! uses the browser cookie header copied from a logged-in music.youtube.com tab.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::{
    db::Db,
    models::{parse_duration, ArtistPage, ArtistShelf, BrowseRef, GridPage, HomeItem, SearchResults, Shelf, Track},
    services::{cache, profile, ytlog},
};

const BASE: &str = "https://music.youtube.com/youtubei/v1/";
const ORIGIN: &str = "https://music.youtube.com";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0";
/// Search params for the "Songs" filter.
/// Setting: when YouTube started answering the saved sign-in as signed out (it expired).
pub const SESSION_EXPIRED_KEY: &str = "ytmusic.session_expired";
/// What the last signed-in answer said: 0 not known yet, 1 signed in, 2 signed out (expired).
static SESSION_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
use std::sync::atomic::Ordering;

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Whether YouTube answered as signed in (`logged_in` in `responseContext`), when it says.
fn response_logged_in(v: &Value) -> Option<bool> {
    v["responseContext"]["serviceTrackingParams"]
        .as_array()?
        .iter()
        .flat_map(|s| s["params"].as_array().into_iter().flatten())
        .find(|p| p["key"].as_str() == Some("logged_in"))
        .and_then(|p| p["value"].as_str())
        .map(|v| v == "1")
}

const SONGS_FILTER: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
const VIDEOS_FILTER: &str = "EgWKAQIQAWoKEAkQBRAKEAMQBA%3D%3D";
const ARTISTS_FILTER: &str = "EgWKAQIgAWoSEAUQCRAKEAMQEBAEEBUQDhAR";
const ALBUMS_FILTER: &str = "EgWKAQIYAWoSEAUQCRAKEAMQEBAEEBUQDhAR";
pub const COOKIE_KEY: &str = "ytmusic.cookie";
/// Where the cookie came from: "manual", or "browser:<name>" when imported (and kept in sync)
/// from a browser on the server machine.
pub const COOKIE_SOURCE_KEY: &str = "ytmusic.cookie_source";
pub const COOKIE_SYNCED_KEY: &str = "ytmusic.cookie_synced_at";
pub const ACCOUNT_KEY: &str = "ytmusic.cookie_account";

/// Browsers tried, in order, when importing a sign-in.
pub const BROWSERS: &[&str] = &["firefox", "chrome", "chromium", "brave", "edge", "vivaldi", "opera"];

/// Build the `Cookie` header music.youtube.com would receive from a browser's cookie jar.
/// Returns None unless the jar holds a signed-in YouTube session.
pub fn cookie_header(cookies: &[crate::services::ytdlp::BrowserCookie]) -> Option<String> {
    let mut picked: Vec<(&str, &str, usize)> = Vec::new(); // (name, value, specificity)
    for c in cookies {
        let d = c.domain.trim_start_matches('.');
        let specificity = match d {
            "music.youtube.com" => 2,
            "youtube.com" => 1,
            _ => continue,
        };
        match picked.iter_mut().find(|(n, _, _)| *n == c.name) {
            Some(p) if p.2 < specificity => *p = (&c.name, &c.value, specificity),
            Some(_) => {}
            None => picked.push((&c.name, &c.value, specificity)),
        }
    }
    let header = picked.iter().map(|(n, v, _)| format!("{n}={v}")).collect::<Vec<_>>().join("; ");
    sapisid(&header).map(|_| header)
}

#[derive(Clone)]
pub struct YtMusic {
    http: reqwest::Client,
    db: Db,
    /// When Google starts refusing us ("unusual traffic"), every request waits until this passes.
    cooldown: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// Consecutive "unusual traffic" refusals; the pause grows with each, and resets on success.
    strikes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Spacing for background requests (imports, retries, syncs): one at a time, with a gap.
    pacer: std::sync::Arc<tokio::sync::Mutex<std::time::Instant>>,
    /// Multiplier on the gap: doubles when YouTube refuses us, eases back on success.
    pace_factor: std::sync::Arc<std::sync::Mutex<f64>>,
    /// Base gap between background requests (from the import pace setting).
    pace_base_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Signature timestamp of YouTube's current player script, and when we read it.
    sts: std::sync::Arc<tokio::sync::Mutex<Option<(u32, std::time::Instant)>>>,
}

/// Setting for how far apart background requests are: "original" | "normal" | "slow" | "very_slow".
/// "original" is the behaviour before any rate-limit handling: no spacing between requests
/// (just a short pause per song in the import loop), one attempt each, no retries or pauses.
pub const IMPORT_PACE_KEY: &str = "prefs.import_pace";
pub const DEFAULT_IMPORT_PACE: &str = "slow";
/// Base gap for a pace setting; each request also waits a random extra of up to 60% of it.
pub fn pace_base_ms(pace: &str) -> Option<u64> {
    match pace {
        "original" => Some(0),
        "normal" => Some(3_000),
        "slow" => Some(6_000),
        "very_slow" => Some(12_000),
        _ => None,
    }
}
const PACE_MAX_FACTOR: f64 = 4.0;

/// Pause after the 1st, 2nd, … consecutive refusal. Often a single refusal clears up within
/// seconds, so start short.
const BLOCK_PAUSE_SECS: [u64; 5] = [3, 15, 60, 120, 300];
/// Someone waiting on a screen never waits longer than this for a pause to end.
const INTERACTIVE_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

impl YtMusic {
    pub fn new(http: reqwest::Client, db: Db) -> Self {
        Self {
            http,
            db,
            cooldown: Default::default(),
            strikes: Default::default(),
            pacer: std::sync::Arc::new(tokio::sync::Mutex::new(std::time::Instant::now())),
            pace_factor: std::sync::Arc::new(std::sync::Mutex::new(1.0)),
            pace_base_ms: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(pace_base_ms(DEFAULT_IMPORT_PACE).unwrap_or(6_000))),
            sts: Default::default(),
        }
    }

    /// Apply the import pace setting ("normal" | "slow" | "very_slow").
    pub fn set_pace(&self, pace: &str) {
        if let Some(ms) = pace_base_ms(pace) {
            self.pace_base_ms.store(ms, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Background work runs the way it did originally (see [`IMPORT_PACE_KEY`]).
    pub fn original_pace(&self) -> bool {
        self.pace_base_ms.load(std::sync::atomic::Ordering::Relaxed) == 0
    }

    /// Pause between songs in import loops: only used at the original pace, where the
    /// per-request spacing is off.
    pub fn song_gap(&self) -> std::time::Duration {
        std::time::Duration::from_millis(if self.original_pace() { 120 } else { 0 })
    }

    fn pace_base(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.pace_base_ms.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Wait for this background request's turn. Requests go one at a time, spaced out.
    async fn pace(&self) {
        let mut next = self.pacer.lock().await; // held while waiting: background requests queue up
        let now = std::time::Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        let factor = *self.pace_factor.lock().unwrap_or_else(|p| p.into_inner());
        let base = self.pace_base();
        let jitter = base.mul_f64(0.6 * rand::thread_rng().gen::<f64>());
        *next = std::time::Instant::now() + (base + jitter).mul_f64(factor);
    }

    fn adjust_pace(&self, refused: bool) {
        let mut f = self.pace_factor.lock().unwrap_or_else(|p| p.into_inner());
        let old = *f;
        *f = if refused { (old * 2.0).min(PACE_MAX_FACTOR) } else { (old * 0.97).max(1.0) };
        if refused && *f != old {
            tracing::info!("slowing background YouTube requests: ~{:.1}s apart", self.pace_base().as_secs_f64() * 1.3 * *f);
        }
    }

    /// Current spacing between background requests (for the request log), in ms.
    pub fn pace_gap_ms(&self) -> u64 {
        let f = *self.pace_factor.lock().unwrap_or_else(|p| p.into_inner());
        if self.original_pace() {
            return 120;
        }
        (self.pace_base().as_millis() as f64 * 1.3 * f) as u64
    }

    async fn wait_for_cooldown(&self, endpoint: &str, detail: &str) {
        let until = *self.cooldown.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = until {
            let now = std::time::Instant::now();
            if t > now {
                let timer = ytlog::start("cooldown", endpoint, detail);
                let mut wait = t - now;
                if ytlog::is_interactive() {
                    wait = wait.min(INTERACTIVE_MAX_WAIT);
                }
                tokio::time::sleep(wait).await;
                timer.done("waited", None, 0, None);
            }
        }
    }

    fn start_cooldown(&self, secs: u64) {
        let mut c = self.cooldown.lock().unwrap_or_else(|p| p.into_inner());
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        if c.map_or(true, |t| t < until) {
            *c = Some(until);
        }
    }

    /// The saved Google sign-in, regardless of the active profile.
    pub async fn stored_cookie(&self) -> Option<String> {
        self.db.get_setting(COOKIE_KEY).await.ok().flatten()
    }

    async fn fresh_profile(&self) -> bool {
        self.db.active_profile().await == profile::FRESH
    }

    /// The sign-in to use right now: none while the fresh profile is active.
    pub async fn cookie(&self) -> Option<String> {
        if self.fresh_profile().await {
            return None;
        }
        self.stored_cookie().await
    }

    /// YouTube doesn't refuse an expired sign-in, it answers as if signed out (saying so in
    /// `responseContext`). Each signed-in answer updates whether that's happening, remembered
    /// under [`SESSION_EXPIRED_KEY`] (the time it was first seen) until a signed-in answer again.
    async fn note_session(&self, v: &Value) {
        let Some(logged_in) = response_logged_in(v) else { return };
        let state = if logged_in { 1 } else { 2 };
        if SESSION_STATE.swap(state, Ordering::Relaxed) == state {
            return;
        }
        if logged_in {
            let _ = self.db.delete_settings(SESSION_EXPIRED_KEY).await;
        } else if self.db.get_setting(SESSION_EXPIRED_KEY).await.ok().flatten().is_none() {
            tracing::warn!("YouTube treats the saved sign-in as signed out: it has expired");
            let _ = self.db.set_setting(SESSION_EXPIRED_KEY, &now_secs().to_string()).await;
        }
    }

    /// A new sign-in was saved: forget what the old one did.
    pub async fn reset_session_state(&self) {
        SESSION_STATE.store(0, Ordering::Relaxed);
        let _ = self.db.delete_settings(SESSION_EXPIRED_KEY).await;
    }

    /// Since when YouTube has been treating the saved sign-in as signed out (expired), if it is.
    pub async fn session_expired_since(&self) -> Option<i64> {
        self.db.get_setting(SESSION_EXPIRED_KEY).await.ok().flatten().and_then(|s| s.parse().ok())
    }

    /// Whether a Google sign-in is saved (shown in Settings).
    pub async fn is_authenticated(&self) -> bool {
        self.stored_cookie().await.map(|c| sapisid(&c).is_some()).unwrap_or(false)
    }

    /// Whether requests are currently made as the signed-in account.
    pub async fn signed_in_now(&self) -> bool {
        self.cookie().await.map(|c| sapisid(&c).is_some()).unwrap_or(false)
    }

    /// Credentials for the active profile: the Google cookie, or for the fresh profile an
    /// anonymous visitor id of its own (so YouTube treats it as one consistent new listener).
    async fn active_creds(&self) -> Creds {
        if self.fresh_profile().await {
            Creds::Visitor(self.db.get_setting(profile::FRESH_VISITOR_KEY).await.ok().flatten())
        } else {
            self.stored_cookie().await.map(Creds::Cookie).unwrap_or(Creds::Anonymous)
        }
    }

    async fn post(&self, endpoint: &str, body: Value, auth: bool) -> anyhow::Result<Value> {
        let creds = if auth { self.active_creds().await } else { Creds::Anonymous };
        self.post_with(endpoint, body, creds).await
    }

    /// A `context.client.visitorData` already in `body` is kept (continuations need the visitor
    /// id of the session that got the token).
    async fn post_with(&self, endpoint: &str, mut body: Value, creds: Creds) -> anyhow::Result<Value> {
        let given_visitor = body["context"]["client"]["visitorData"].as_str().map(String::from);
        let mut client = json!({
            "clientName": "WEB_REMIX",
            "clientVersion": client_version(),
            "hl": "en",
            "gl": "US",
        });
        if let Creds::Visitor(Some(v)) = &creds {
            client["visitorData"] = json!(v);
        } else if let Some(v) = given_visitor {
            client["visitorData"] = json!(v);
        }
        body["context"] = json!({ "client": client, "user": {} });

        // Bulk work (imports) can trip YouTube's rate limits: back off and retry instead of
        // treating a refused request as "no result". A screen waiting on the answer gets one
        // quick retry and then an error (the cache serves the last good copy where it has one).
        let detail = request_detail(&body);
        let interactive = ytlog::is_interactive();
        // Original pace: background requests go out immediately, once, and a refusal is just an error.
        let original = !interactive && self.original_pace();
        let max_blocked = if original { 0 } else if interactive { 1 } else { 4 };
        let max_retries = if original { 0 } else if interactive { 1 } else { 4 };
        let mut attempt = 0;
        loop {
            if !original {
                self.wait_for_cooldown(endpoint, &detail).await;
            }
            if !interactive && !original {
                self.pace().await;
            }
            match self.send_once(endpoint, &body, &creds, &detail, attempt + 1).await {
                Ok(v) => {
                    self.strikes.store(0, std::sync::atomic::Ordering::Relaxed);
                    self.adjust_pace(false);
                    // A new fresh-profile visitor: remember the id YouTube assigned so it stays the same listener.
                    if let Creds::Visitor(None) = creds {
                        if let Some(id) = v["responseContext"]["visitorData"].as_str() {
                            let _ = self.db.set_setting(profile::FRESH_VISITOR_KEY, id).await;
                        }
                    }
                    return Ok(v);
                }
                Err(SendError::RateLimited(_)) if attempt < max_blocked => {
                    // Google's "unusual traffic" block: pause *all* requests, longer each time in a row.
                    let n = self.strikes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    self.adjust_pace(true);
                    let secs = BLOCK_PAUSE_SECS[n.min(BLOCK_PAUSE_SECS.len() - 1)];
                    tracing::warn!("YouTube is rate-limiting us ({endpoint}); pausing requests for {secs}s");
                    self.start_cooldown(secs);
                    attempt += 1;
                }
                Err(SendError::RateLimited(e)) => {
                    if !original {
                        self.strikes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        self.adjust_pace(true);
                    }
                    return Err(e);
                }
                // Timeouts / server errors: background jobs retry patiently, screens retry once.
                Err(SendError::Retryable(e)) if attempt < max_retries => {
                    let wait = std::time::Duration::from_millis(1000 * 3u64.pow(attempt));
                    tracing::debug!("youtube music {endpoint}: {e:#}; retrying in {wait:?}");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                Err(SendError::Retryable(e)) | Err(SendError::Fatal(e)) => return Err(e),
            }
        }
    }

    async fn send_once(&self, endpoint: &str, body: &Value, creds: &Creds, detail: &str, attempt: u32) -> Result<Value, SendError> {
        let timer = ytlog::start("innertube", endpoint, format!("{detail} [{}]", creds.label()));
        let mut req = self
            .http
            .post(format!("{BASE}{endpoint}?alt=json&prettyPrint=false"))
            .header("user-agent", UA)
            .header("accept", "*/*")
            .header("origin", ORIGIN)
            .header("x-origin", ORIGIN)
            .header("referer", format!("{ORIGIN}/"))
            .header("x-goog-authuser", "0")
            .timeout(std::time::Duration::from_secs(20))
            .json(body);
        match creds {
            Creds::Cookie(cookie) => {
                if let Some(sid) = sapisid(cookie) {
                    req = req.header("authorization", sapisid_hash(&sid));
                }
                req = req.header("cookie", cookie.clone());
            }
            Creds::Visitor(Some(v)) => req = req.header("x-goog-visitor-id", v.clone()),
            _ => {}
        }
        if !matches!(creds, Creds::Visitor(Some(_))) {
            if let Some(v) = body["context"]["client"]["visitorData"].as_str() {
                req = req.header("x-goog-visitor-id", v);
            }
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                timer.done(if e.is_timeout() { "timeout" } else { "network-error" }, None, attempt, Some(e.to_string()));
                return Err(SendError::Retryable(e.into()));
            }
        };
        let status = resp.status();
        let code = Some(status.as_u16());
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            // Google answers bot detection with a 403/429 "Sorry…" HTML page.
            let blocked = status.as_u16() == 429 || (status.as_u16() == 403 && text.contains("Sorry"));
            if blocked {
                timer.done("rate-limited", code, attempt, None);
                return Err(SendError::RateLimited(anyhow!(
                    "YouTube is temporarily rate-limiting this server ({status}); try again in a few minutes"
                )));
            }
            let snippet = text.chars().take(300).collect::<String>();
            timer.done("http-error", code, attempt, Some(snippet.chars().take(120).collect()));
            let err = anyhow!("youtube music {endpoint} failed: {status} {snippet}");
            return Err(if status.is_server_error() { SendError::Retryable(err) } else { SendError::Fatal(err) });
        }
        match resp.json::<Value>().await {
            Ok(v) => {
                timer.done("ok", code, attempt, None);
                if matches!(creds, Creds::Cookie(_)) {
                    self.note_session(&v).await;
                }
                Ok(v)
            }
            Err(e) => {
                timer.done(if e.is_timeout() { "timeout" } else { "error" }, code, attempt, Some(format!("reading body: {e}")));
                Err(SendError::Retryable(e.into()))
            }
        }
    }

    /// Cache key scoped to who's asking (profile + signed in or not), since results are personalised.
    async fn ckey(&self, kind: &str, arg: &str) -> String {
        let p = self.db.active_profile().await;
        let signed = self.signed_in_now().await as u8;
        format!("yt:{p}:{signed}:{kind}:{arg}")
    }

    // ---- public operations (cached server-side) ---------------------------

    pub async fn search_songs(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let key = self.ckey("search", &format!("{limit}:{}", query.trim().to_lowercase())).await;
        cache::cached(&self.db, &key, 6 * cache::HOUR, false, || self.fetch_search_songs(query, limit)).await
    }

    pub async fn watch_playlist(&self, video_id: Option<&str>, playlist_id: Option<&str>, limit: usize) -> anyhow::Result<Vec<Track>> {
        let key = self
            .ckey("watch", &format!("{}:{}:{limit}", video_id.unwrap_or(""), playlist_id.unwrap_or("")))
            .await;
        cache::cached(&self.db, &key, 6 * cache::HOUR, false, || self.fetch_watch_playlist(video_id, playlist_id, limit)).await
    }

    /// Home feed's first page; `refresh` bypasses the cache (pull-to-refresh).
    pub async fn home(&self, refresh: bool) -> anyhow::Result<HomePage> {
        let key = self.ckey("home-page", "").await;
        cache::cached(&self.db, &key, 20 * cache::MINUTE, refresh, || self.fetch_home()).await
    }

    /// The next page of the home feed, as YouTube Music loads it when you scroll down
    /// (`continuation` from the page before). Cached per token as long as the first page, so
    /// scrolling down again costs no YouTube calls.
    pub async fn home_more(&self, continuation: &str) -> anyhow::Result<HomePage> {
        let digest: String = Sha1::digest(continuation.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let key = self.ckey("home-more", &digest).await;
        cache::cached(&self.db, &key, 20 * cache::MINUTE, false, || async move {
            let (visitor, token) = continuation.split_once('|').context("invalid continuation")?;
            let visitor = (!visitor.is_empty()).then_some(visitor);
            let v = self.post_continuation(token, visitor).await?;
            Ok(HomePage { shelves: parse_shelves(&v), continuation: home_continuation(&v, visitor) })
        })
        .await
    }

    /// A feed continuation. YouTube ties the token to the session that got it: without the first
    /// page's visitor id the answer is empty unless you're signed in.
    async fn post_continuation(&self, token: &str, visitor: Option<&str>) -> anyhow::Result<Value> {
        let mut body = json!({ "continuation": token });
        if let Some(v) = visitor {
            body["context"] = json!({ "client": { "visitorData": v } });
        }
        self.post("browse", body, true).await
    }

    pub async fn playlist_tracks(&self, id: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        // Your liked songs change often; albums practically never.
        let ttl = if id == "LM" {
            10 * cache::MINUTE
        } else if id.starts_with("MPRE") {
            7 * cache::DAY
        } else {
            12 * cache::HOUR
        };
        let key = self.ckey("playlist", &format!("{id}:{limit}")).await;
        cache::cached(&self.db, &key, ttl, false, || self.fetch_playlist_tracks(id, limit)).await
    }

    /// A podcast's newest episodes: one YouTube call, plus the token for the next page (see
    /// [`Self::episodes_more`]), loaded only as you scroll.
    ///
    /// A show (`MPSPPL…`) is read as its playlist (`VLPL…`): the same episodes, newest first, in
    /// the playlist layout we parse, with exact lengths (the show page only has "34 min").
    pub async fn podcast_episodes(&self, id: &str) -> anyhow::Result<TrackPage> {
        let pl = id.strip_prefix("MPSP").unwrap_or(id).to_string();
        let key = self.ckey("podcast", &pl).await;
        cache::cached(&self.db, &key, PODCAST_TTL, false, || async move {
            let v = self.post("browse", json!({ "browseId": format!("VL{pl}") }), true).await?;
            Ok(track_page(&v))
        })
        .await
    }

    /// The next page of a podcast's episodes (one YouTube call), from the page before's token.
    pub async fn episodes_more(&self, token: &str) -> anyhow::Result<TrackPage> {
        let digest: String = Sha1::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let key = self.ckey("podcast-more", &digest).await;
        cache::cached(&self.db, &key, PODCAST_TTL, false, || async move {
            let v = self.post("browse", json!({ "continuation": token }), true).await?;
            Ok(track_page(&v))
        })
        .await
    }

    /// A playlist's, album's or podcast's header (its name, for pages opened from a pasted link).
    pub async fn playlist_info(&self, id: &str) -> anyhow::Result<HeaderInfo> {
        // A podcast's own page has the show's name and publisher.
        let browse_id = if id.starts_with("MPSP") { id.to_string() } else { playlist_browse_id(id) };
        let key = self.ckey("playlist-info", id).await;
        cache::cached(&self.db, &key, 12 * cache::HOUR, false, || async move {
            let v = self.post("browse", json!({ "browseId": browse_id }), true).await?;
            Ok(header_info(&v))
        })
        .await
    }

    pub async fn song_info(&self, video_id: &str) -> anyhow::Result<Track> {
        let key = format!("yt:song:{video_id}");
        cache::cached(&self.db, &key, 30 * cache::DAY, false, || self.fetch_song_info(video_id)).await
    }

    /// Search everything (`filter`: None, "artists" or "albums"): top result, artists, albums,
    /// songs, videos, playlists.
    pub async fn search_all(&self, query: &str, filter: Option<&str>) -> anyhow::Result<SearchResults> {
        let params = match filter {
            None => None,
            Some("artists") => Some(ARTISTS_FILTER),
            Some("albums") => Some(ALBUMS_FILTER),
            Some(f) => bail!("unknown search filter {f}"),
        };
        let key = self.ckey("search_all", &format!("{}:{}", filter.unwrap_or("all"), query.trim().to_lowercase())).await;
        cache::cached(&self.db, &key, 6 * cache::HOUR, false, || async move {
            let mut body = json!({ "query": query });
            if let Some(p) = params {
                body["params"] = json!(p);
            }
            let v = self.post("search", body, true).await?;
            Ok(parse_search(&v))
        })
        .await
    }

    /// An artist's page: header, top songs, albums, singles, videos, related artists…
    pub async fn artist(&self, browse_id: &str) -> anyhow::Result<ArtistPage> {
        let key = self.ckey("artist", browse_id).await;
        cache::cached(&self.db, &key, 12 * cache::HOUR, false, || async move {
            let v = self.post("browse", json!({ "browseId": browse_id }), true).await?;
            parse_artist(&v, browse_id).context("not an artist page")
        })
        .await
    }

    /// A grid page such as an artist's full list of albums or singles.
    pub async fn browse_grid(&self, browse_id: &str, params: Option<&str>) -> anyhow::Result<GridPage> {
        let key = self.ckey("grid", &format!("{browse_id}:{}", params.unwrap_or(""))).await;
        cache::cached(&self.db, &key, cache::DAY, false, || async move {
            let mut body = json!({ "browseId": browse_id });
            if let Some(p) = params {
                body["params"] = json!(urlencoding::decode(p).map(|c| c.into_owned()).unwrap_or_else(|_| p.to_string()));
            }
            let v = self.post("browse", body, true).await?;
            let mut items = Vec::new();
            find_all(&v, "musicTwoRowItemRenderer", &mut items);
            Ok(GridPage {
                title: header_info(&v).title.unwrap_or_default(),
                items: items.into_iter().filter_map(parse_two_row).collect(),
            })
        })
        .await
    }

    /// The artist page id for a name (for "Go to artist" from a song, which only has the name).
    pub async fn artist_id_for(&self, name: &str) -> anyhow::Result<Option<String>> {
        let found = self.search_all(name, Some("artists")).await?;
        let want = crate::services::matching::normalize(name);
        let ids: Vec<(String, String)> = found
            .top
            .iter()
            .chain(found.artists.iter())
            .filter(|a| a.kind == "artist")
            .filter_map(|a| Some((crate::services::matching::normalize(&a.title), a.browse_id.clone()?)))
            .collect();
        // Prefer an exact name match; otherwise YouTube's first artist.
        Ok(ids.iter().find(|(n, _)| *n == want).or(ids.first()).map(|(_, id)| id.clone()))
    }

    /// Videos only (uploads that aren't in YouTube Music's song catalogue: demos, mixes, live sets).
    pub async fn search_videos(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let key = self.ckey("search-videos", &format!("{limit}:{}", query.trim().to_lowercase())).await;
        cache::cached(&self.db, &key, 6 * cache::HOUR, false, || async move {
            let params = urlencoding::decode(VIDEOS_FILTER)?.into_owned();
            let v = self.post("search", json!({ "query": query, "params": params }), true).await?;
            let mut out = parse_responsive_items(&v);
            out.truncate(limit);
            Ok(out)
        })
        .await
    }

    async fn fetch_search_songs(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let params = urlencoding::decode(SONGS_FILTER)?.into_owned();
        let v = self.post("search", json!({ "query": query, "params": params }), true).await?;
        let mut out = parse_responsive_items(&v);
        out.truncate(limit);
        Ok(out)
    }

    /// "Radio"/up-next list for a video or playlist. This is what YouTube Music
    /// uses for its automatic queue and the core of our recommendations.
    async fn fetch_watch_playlist(
        &self,
        video_id: Option<&str>,
        playlist_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<Track>> {
        let mut body = json!({
            "enablePersistentPlaylistPanel": true,
            "isAudioOnly": true,
            "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
        });
        match (video_id, playlist_id) {
            (Some(v), Some(p)) => {
                body["videoId"] = json!(v);
                body["playlistId"] = json!(p);
            }
            (Some(v), None) => {
                body["videoId"] = json!(v);
                body["playlistId"] = json!(format!("RDAMVM{v}"));
                body["watchEndpointMusicSupportedConfigs"] = json!({
                    "watchEndpointMusicConfig": { "hasPersistentPlaylistPanel": true, "musicVideoType": "MUSIC_VIDEO_TYPE_ATV" }
                });
            }
            (None, Some(p)) => body["playlistId"] = json!(p),
            (None, None) => bail!("video_id or playlist_id required"),
        }

        let v = self.post("next", body, true).await?;
        let mut tracks = parse_panel_items(&v);
        // Follow continuations for longer mixes.
        let mut token = find_continuation(&v);
        let mut pages = 0;
        while tracks.len() < limit && pages < 5 {
            let Some(t) = token.take() else { break };
            let next = self
                .post("next", json!({ "continuation": t, "enablePersistentPlaylistPanel": true, "isAudioOnly": true }), true)
                .await?;
            let more = parse_panel_items(&next);
            if more.is_empty() {
                break;
            }
            tracks.extend(more);
            token = find_continuation(&next);
            pages += 1;
        }
        dedupe(&mut tracks);
        tracks.truncate(limit);
        Ok(tracks)
    }

    /// The first page plus one continuation, so the page is full on open (as before); the rest
    /// loads as you scroll.
    async fn fetch_home(&self) -> anyhow::Result<HomePage> {
        let v = self.post("browse", json!({ "browseId": "FEmusic_home" }), true).await?;
        let visitor = v["responseContext"]["visitorData"].as_str().map(String::from);
        let visitor = visitor.as_deref();
        let mut shelves = parse_shelves(&v);
        let mut continuation = home_continuation(&v, visitor);
        if let Some(token) = feed_continuation(&v) {
            match self.post_continuation(&token, visitor).await {
                Ok(more) => {
                    shelves.extend(parse_shelves(&more));
                    continuation = home_continuation(&more, visitor);
                }
                // Scrolling down retries it.
                Err(e) => tracing::warn!("home feed page 2: {e:#}"),
            }
        }
        Ok(HomePage { shelves, continuation })
    }

    /// Tracks of a playlist ("PL..", "VL..", "RD..", "LM") or album ("MPRE..").
    async fn fetch_playlist_tracks(&self, id: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        if id.starts_with("RD") {
            return self.fetch_watch_playlist(None, Some(id), limit).await;
        }
        let browse_id = playlist_browse_id(id);
        let v = self.post("browse", json!({ "browseId": browse_id }), true).await?;
        let header = header_info(&v);
        let mut tracks = parse_responsive_items(&v);
        let mut token = list_continuation(&v);
        let mut pages = 0;
        while tracks.len() < limit && pages < 200 {
            let Some(t) = token.take() else { break };
            let next = self.post("browse", json!({ "continuation": t }), true).await?;
            let more = parse_responsive_items(&next);
            if more.is_empty() {
                break;
            }
            tracks.extend(more);
            token = list_continuation(&next);
            pages += 1;
        }
        // Album tracks don't carry artist/thumbnail per row: inherit from the header.
        for t in &mut tracks {
            if t.artist.is_empty() {
                t.artist = header.artist.clone().unwrap_or_default();
            }
            if t.thumbnail.is_none() {
                t.thumbnail = header.thumbnail.clone();
            }
            if t.album.is_none() && browse_id.starts_with("MPRE") {
                t.album = header.title.clone();
            }
        }
        dedupe(&mut tracks);
        tracks.truncate(limit);
        Ok(tracks)
    }

    /// The signed-in account's name, or None if YouTube treats the request as signed out.
    ///
    /// YouTube doesn't reject invalid credentials — it silently answers as a logged-out
    /// visitor — so this is the only reliable way to tell whether a cookie works.
    pub async fn account_name(&self) -> anyhow::Result<Option<String>> {
        // Always checks the saved sign-in, whichever profile is active.
        let Some(cookie) = self.stored_cookie().await else { return Ok(None) };
        let v = self.post_with("account/account_menu", json!({}), Creds::Cookie(cookie)).await?;
        Ok(find_first(&v, "activeAccountHeaderRenderer").map(|h| {
            let name = text(&h["accountName"]);
            let handle = text(&h["channelHandle"]);
            match (name.is_empty(), handle.is_empty()) {
                (false, false) => format!("{name} ({handle})"),
                (false, true) => name,
                (true, false) => handle,
                (true, true) => "your Google account".to_string(),
            }
        }))
    }

    pub async fn liked_songs(&self, limit: usize) -> anyhow::Result<Vec<Track>> {
        if !self.signed_in_now().await {
            bail!("Liked songs need your YouTube Music sign-in (switch off Fresh start, or sign in in Settings)");
        }
        self.playlist_tracks("LM", limit).await
    }

    /// Metadata for a single video via the player endpoint.
    async fn fetch_song_info(&self, video_id: &str) -> anyhow::Result<Track> {
        let v = self.player(video_id).await?;
        let d = &v["videoDetails"];
        let title = d["title"].as_str().ok_or_else(|| anyhow!("no videoDetails for {video_id}"))?;
        Ok(Track {
            video_id: video_id.to_string(),
            title: title.to_string(),
            artist: d["author"].as_str().unwrap_or_default().trim_end_matches(" - Topic").to_string(),
            album: None,
            duration: d["lengthSeconds"].as_str().and_then(|s| s.parse().ok()),
            thumbnail: last_thumbnail(&d["thumbnail"]),
        })
    }

    async fn player(&self, video_id: &str) -> anyhow::Result<Value> {
        let body = self.player_body(video_id).await;
        self.post("player", body, true).await
    }

    async fn player_body(&self, video_id: &str) -> Value {
        json!({
            "videoId": video_id,
            "playbackContext": { "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS",
                "signatureTimestamp": self.signature_timestamp().await,
            } },
            "contentCheckOk": true,
            "racyCheckOk": true,
        })
    }

    /// The `signatureTimestamp` of YouTube's current player script.
    ///
    /// The player endpoint answers "UNPLAYABLE: Video unavailable" (and leaves out the
    /// playback tracking URLs that history reports need) unless the request carries the
    /// timestamp of the live player script. It changes every few days; re-read every 12 hours.
    async fn signature_timestamp(&self) -> u32 {
        const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(12 * 3600);
        const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);
        let mut cached = self.sts.lock().await;
        if let Some((sts, at)) = *cached {
            if at.elapsed() < if sts == 0 { RETRY_AFTER } else { MAX_AGE } {
                return sts;
            }
        }
        let fresh = match self.fetch_signature_timestamp().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("couldn't read YouTube's player signature timestamp: {e:#}");
                // Keep using the last known value, if any; otherwise retry in a few minutes.
                cached.map(|(v, _)| v).unwrap_or(0)
            }
        };
        *cached = Some((fresh, std::time::Instant::now()));
        fresh
    }

    async fn fetch_signature_timestamp(&self) -> anyhow::Result<u32> {
        let get = |url: String| async move {
            let timer = ytlog::start("playerjs", "fetch", url.clone());
            let r = self.http.get(&url).header("user-agent", UA).timeout(std::time::Duration::from_secs(20)).send().await;
            let r = match r {
                Ok(r) => r,
                Err(e) => {
                    timer.done(if e.is_timeout() { "timeout" } else { "network-error" }, None, 1, Some(e.to_string()));
                    return Err(anyhow::Error::from(e));
                }
            };
            let code = r.status().as_u16();
            if !r.status().is_success() {
                timer.done("http-error", Some(code), 1, None);
                bail!("{url}: HTTP {code}");
            }
            timer.done("ok", Some(code), 1, None);
            Ok(r.text().await?)
        };
        let page = get(format!("{ORIGIN}/")).await?;
        let js_url = regex::Regex::new(r#""jsUrl":"([^"]+)""#)?
            .captures(&page)
            .map(|c| c[1].to_string())
            .context("no player script URL on the YouTube Music page")?;
        let js = get(format!("{ORIGIN}{js_url}")).await?;
        let sts = regex::Regex::new(r"(?:signatureTimestamp|sts)\s*:\s*(\d{5})")?
            .captures(&js)
            .and_then(|c| c[1].parse().ok())
            .context("no signature timestamp in the player script")?;
        tracing::info!("YouTube player signature timestamp: {sts}");
        Ok(sts)
    }

    /// Register a play in the user's YouTube / YouTube Music history.
    ///
    /// Mirrors what the web player does: fetch the player response for the
    /// video and ping its `videostatsPlaybackUrl` with a client playback nonce.
    /// Needs the Google sign-in: YouTube doesn't return playback tracking to anonymous visitors.
    pub async fn add_history(&self, video_id: &str) -> anyhow::Result<()> {
        let creds = self.active_creds().await;
        if !matches!(creds, Creds::Cookie(_)) {
            bail!("YouTube history needs your YouTube Music sign-in");
        }
        let body = self.player_body(video_id).await;
        let v = self.post_with("player", body, self.active_creds().await).await?;
        let Some(url) = v["playbackTracking"]["videostatsPlaybackUrl"]["baseUrl"].as_str() else {
            let status = v["playabilityStatus"]["status"].as_str().unwrap_or("?");
            let reason = v["playabilityStatus"]["reason"].as_str().unwrap_or("no reason given");
            // A stale signature timestamp is the usual cause: read it again next time.
            *self.sts.lock().await = None;
            bail!("YouTube's player response has no history URL ({status}: {reason})");
        };
        let cpn: String = {
            const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut rng = rand::thread_rng();
            (0..16).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
        };
        let mut req = self
            .http
            .get(url)
            .query(&[("ver", "2"), ("c", "WEB_REMIX"), ("cpn", cpn.as_str())])
            .header("user-agent", UA)
            .header("origin", ORIGIN);
        match &creds {
            Creds::Cookie(cookie) => {
                req = req.header("cookie", cookie.clone());
                if let Some(sid) = sapisid(cookie) {
                    req = req.header("authorization", sapisid_hash(&sid));
                }
            }
            _ => {}
        }
        let timer = ytlog::start("history", "videostats", video_id);
        let resp = match req.timeout(std::time::Duration::from_secs(20)).send().await {
            Ok(r) => r,
            Err(e) => {
                timer.done(if e.is_timeout() { "timeout" } else { "network-error" }, None, 1, Some(e.to_string()));
                return Err(e.into());
            }
        };
        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            timer.done("http-error", Some(code), 1, None);
            bail!("history ping failed: {}", resp.status());
        }
        timer.done("ok", Some(code), 1, None);
        Ok(())
    }
}

// ---- auth helpers ---------------------------------------------------------

enum SendError {
    /// Google's bot detection kicked in: back off for a while.
    RateLimited(anyhow::Error),
    /// Server error or network hiccup: worth retrying.
    Retryable(anyhow::Error),
    Fatal(anyhow::Error),
}

/// What a request was about, for the request log.
fn request_detail(body: &Value) -> String {
    for k in ["query", "videoId", "browseId", "playlistId"] {
        if let Some(v) = body[k].as_str() {
            return v.chars().take(80).collect();
        }
    }
    if body.get("continuation").is_some() {
        return "continuation".into();
    }
    String::new()
}

impl Creds {
    fn label(&self) -> &'static str {
        match self {
            Creds::Cookie(_) => "signed-in",
            Creds::Visitor(_) => "fresh",
            Creds::Anonymous => "anon",
        }
    }
}

enum Creds {
    /// Signed in with the saved Google cookie.
    Cookie(String),
    /// Fresh profile: an anonymous visitor (None until YouTube assigns an id).
    Visitor(Option<String>),
    /// Plain logged-out request.
    Anonymous,
}

fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    cookie.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn sapisid(cookie: &str) -> Option<String> {
    cookie_value(cookie, "__Secure-3PAPISID").or_else(|| cookie_value(cookie, "SAPISID"))
}

fn sapisid_hash(sapisid: &str) -> String {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut h = Sha1::new();
    h.update(format!("{ts} {sapisid} {ORIGIN}"));
    format!("SAPISIDHASH {ts}_{}", hex::encode(h.finalize()))
}

fn client_version() -> String {
    // InnerTube wants a plausible, recent date-based version: 1.YYYYMMDD.01.00
    let days = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 / 86400 - 1;
    let (y, m, d) = civil_from_days(days);
    format!("1.{y:04}{m:02}{d:02}.01.00")
}

/// Howard Hinnant's days-from-epoch -> (year, month, day).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---- JSON walking ---------------------------------------------------------

fn find_all<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if k == key {
                    out.push(child);
                } else {
                    find_all(child, key, out);
                }
            }
        }
        Value::Array(arr) => arr.iter().for_each(|c| find_all(c, key, out)),
        _ => {}
    }
}

fn find_first<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if let Some(x) = map.get(key) {
                return Some(x);
            }
            map.values().find_map(|c| find_first(c, key))
        }
        Value::Array(arr) => arr.iter().find_map(|c| find_first(c, key)),
        _ => None,
    }
}

fn text(v: &Value) -> String {
    if let Some(s) = v["simpleText"].as_str() {
        return s.to_string();
    }
    v["runs"]
        .as_array()
        .map(|runs| runs.iter().filter_map(|r| r["text"].as_str()).collect::<String>())
        .unwrap_or_default()
}

fn last_thumbnail(v: &Value) -> Option<String> {
    let mut all = Vec::new();
    find_all(v, "thumbnails", &mut all);
    all.into_iter()
        .filter_map(|t| t.as_array()?.last()?["url"].as_str())
        .next()
        .map(upscale_thumb)
}

/// googleusercontent thumbnails accept a size suffix; ask for something usable as cover art.
fn upscale_thumb(url: &str) -> String {
    if url.contains("googleusercontent.com") || url.contains("ggpht.com") {
        if let Some(idx) = url.rfind("=w") {
            return format!("{}=w544-h544-l90-rj", &url[..idx]);
        }
    }
    url.to_string()
}

fn page_type(run: &Value) -> Option<&str> {
    run["navigationEndpoint"]["browseEndpoint"]["browseEndpointContextSupportedConfigs"]
        ["browseEndpointContextMusicConfig"]["pageType"]
        .as_str()
}

fn find_continuation(v: &Value) -> Option<String> {
    if let Some(c) = find_first(v, "nextContinuationData") {
        if let Some(s) = c["continuation"].as_str() {
            return Some(s.to_string());
        }
    }
    if let Some(c) = find_first(v, "nextRadioContinuationData") {
        if let Some(s) = c["continuation"].as_str() {
            return Some(s.to_string());
        }
    }
    find_first(v, "continuationCommand").and_then(|c| c["token"].as_str()).map(String::from)
}

/// Splits a byline's runs into (artists, album, duration) using navigation page types,
/// falling back to positional heuristics for runs without links.
fn split_byline(runs: &[&Value]) -> (Vec<String>, Option<String>, Option<u32>) {
    let mut artists = Vec::new();
    let mut album = None;
    let mut duration = None;
    let mut plain = Vec::new();
    for r in runs {
        let t = r["text"].as_str().unwrap_or_default();
        if t.trim().is_empty() || t == " • " || t == " & " || t == ", " {
            continue;
        }
        match page_type(r) {
            Some("MUSIC_PAGE_TYPE_ARTIST") | Some("MUSIC_PAGE_TYPE_USER_CHANNEL") => artists.push(t.to_string()),
            Some("MUSIC_PAGE_TYPE_ALBUM") => album = Some(t.to_string()),
            _ => {
                if let Some(d) = parse_duration(t) {
                    duration = Some(d);
                } else if !matches!(t, "Song" | "Video" | "Episode" | "Single" | "EP" | "Album")
                    && !t.ends_with(" views")
                    && !t.ends_with(" plays")
                    && !(t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()))
                {
                    plain.push(t.to_string());
                }
            }
        }
    }
    if artists.is_empty() {
        if let Some(first) = plain.into_iter().next() {
            artists.push(first);
        }
    }
    (artists, album, duration)
}

fn parse_responsive(item: &Value) -> Option<Track> {
    let video_id = item["playlistItemData"]["videoId"]
        .as_str()
        .or_else(|| find_first(item, "watchEndpoint").and_then(|w| w["videoId"].as_str()))?
        .to_string();
    let cols = item["flexColumns"].as_array()?;
    let title = Some(text(&cols.first()?["musicResponsiveListItemFlexColumnRenderer"]["text"])).filter(|t| !t.is_empty())?;

    let mut runs = Vec::new();
    for c in cols.iter().skip(1) {
        if let Some(rs) = c["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"].as_array() {
            runs.extend(rs.iter());
        }
    }
    let (artists, album, mut duration) = split_byline(&runs);
    if duration.is_none() {
        if let Some(fixed) = item["fixedColumns"].as_array().and_then(|f| f.first()) {
            duration = parse_duration(&text(&fixed["musicResponsiveListItemFixedColumnRenderer"]["text"]));
        }
    }
    Some(Track {
        video_id,
        title,
        artist: artists.join(", "),
        album,
        duration,
        thumbnail: last_thumbnail(&item["thumbnail"]),
    })
}

fn parse_panel(item: &Value) -> Option<Track> {
    let video_id = item["videoId"].as_str()?.to_string();
    let title = text(&item["title"]);
    let runs: Vec<&Value> = item["longBylineText"]["runs"].as_array().map(|r| r.iter().collect()).unwrap_or_default();
    let (artists, album, _) = split_byline(&runs);
    Some(Track {
        video_id,
        title,
        artist: artists.join(", "),
        album,
        duration: parse_duration(&text(&item["lengthText"])),
        thumbnail: last_thumbnail(&item["thumbnail"]),
    })
}

pub fn parse_responsive_items(v: &Value) -> Vec<Track> {
    let mut items = Vec::new();
    find_all(v, "musicResponsiveListItemRenderer", &mut items);
    let mut out: Vec<Track> = items.into_iter().filter_map(parse_responsive).collect();
    dedupe(&mut out);
    out
}

fn parse_panel_items(v: &Value) -> Vec<Track> {
    let mut items = Vec::new();
    find_all(v, "playlistPanelVideoRenderer", &mut items);
    items.into_iter().filter_map(parse_panel).collect()
}

fn parse_two_row(item: &Value) -> Option<HomeItem> {
    let title = text(&item["title"]);
    let subtitle_runs: Vec<&Value> = item["subtitle"]["runs"].as_array().map(|r| r.iter().collect()).unwrap_or_default();
    let subtitle = text(&item["subtitle"]);
    let thumbnail = last_thumbnail(&item["thumbnailRenderer"]);
    let nav = &item["navigationEndpoint"];
    if let Some(w) = nav.get("watchEndpoint") {
        let video_id = w["videoId"].as_str().map(String::from);
        let playlist_id = w["playlistId"].as_str().map(String::from);
        let track = video_id.as_ref().map(|vid| {
            let (artists, album, duration) = split_byline(&subtitle_runs);
            Track { video_id: vid.clone(), title: title.clone(), artist: artists.join(", "), album, duration, thumbnail: thumbnail.clone() }
        });
        return Some(HomeItem {
            kind: if video_id.is_some() { "song" } else { "playlist" }.into(),
            title,
            subtitle,
            thumbnail,
            video_id,
            playlist_id,
            browse_id: None,
            track,
        });
    }
    if let Some(w) = nav.get("watchPlaylistEndpoint") {
        return Some(HomeItem {
            kind: "playlist".into(),
            title,
            subtitle,
            thumbnail,
            video_id: None,
            playlist_id: w["playlistId"].as_str().map(String::from),
            browse_id: None,
            track: None,
        });
    }
    let b = nav.get("browseEndpoint")?;
    let browse_id = b["browseId"].as_str()?.to_string();
    let pt = b["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]["pageType"]
        .as_str()
        .unwrap_or_default();
    let kind = match pt {
        "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK" => "album",
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => "artist",
        _ => "playlist",
    };
    let playlist_id = (kind == "playlist").then(|| browse_id.trim_start_matches("VL").to_string());
    Some(HomeItem { kind: kind.into(), title, subtitle, thumbnail, video_id: None, playlist_id, browse_id: Some(browse_id), track: None })
}

fn parse_shelves(v: &Value) -> Vec<Shelf> {
    let mut shelves = Vec::new();
    find_all(v, "musicCarouselShelfRenderer", &mut shelves);
    shelves
        .into_iter()
        .filter_map(|s| {
            let header = &s["header"]["musicCarouselShelfBasicHeaderRenderer"];
            let title = text(&header["title"]);
            let items: Vec<HomeItem> = s["contents"]
                .as_array()?
                .iter()
                .filter_map(|c| {
                    if let Some(two) = c.get("musicTwoRowItemRenderer") {
                        parse_two_row(two)
                    } else if let Some(r) = c.get("musicResponsiveListItemRenderer") {
                        let t = parse_responsive(r)?;
                        Some(HomeItem {
                            kind: "song".into(),
                            title: t.title.clone(),
                            subtitle: t.artist.clone(),
                            thumbnail: t.thumbnail.clone(),
                            video_id: Some(t.video_id.clone()),
                            playlist_id: None,
                            browse_id: None,
                            track: Some(t),
                        })
                    } else {
                        None
                    }
                })
                .collect();
            (!items.is_empty()).then_some(Shelf { title: if title.is_empty() { "For you".into() } else { title }, items })
        })
        .collect()
}

// ---- search & artist pages ---------------------------------------------------------------

fn browse_ref(nav: &Value) -> Option<BrowseRef> {
    let b = nav.get("browseEndpoint")?;
    Some(BrowseRef { browse_id: b["browseId"].as_str()?.to_string(), params: b["params"].as_str().map(String::from) })
}

fn nav_page_type(nav: &Value) -> Option<&str> {
    nav["browseEndpoint"]["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]["pageType"].as_str()
}

/// What a search result row is, from where it links and its "Song • …" / "Album • …" subtitle.
enum Hit {
    Song(Track),
    Video(Track),
    Artist(HomeItem),
    Album(HomeItem),
    Playlist(HomeItem),
}

fn classify_row(item: &Value) -> Option<Hit> {
    let cols = item["flexColumns"].as_array()?;
    let col = |i: usize| text(&cols.get(i).map(|c| &c["musicResponsiveListItemFlexColumnRenderer"]["text"]).unwrap_or(&Value::Null));
    let title = col(0);
    let subtitle = col(1);
    let mut parts = subtitle.split(" • ");
    let kind_word = parts.next().unwrap_or_default().to_string();
    let rest = parts.collect::<Vec<_>>().join(" • ");
    let thumbnail = last_thumbnail(&item["thumbnail"]);
    let nav = &item["navigationEndpoint"];
    if let Some(r) = browse_ref(nav) {
        let item = |kind: &str, subtitle: String, playlist_id: Option<String>| HomeItem {
            kind: kind.into(),
            title: title.clone(),
            subtitle,
            thumbnail: thumbnail.clone(),
            video_id: None,
            playlist_id,
            browse_id: Some(r.browse_id.clone()),
            track: None,
        };
        return match nav_page_type(nav)? {
            "MUSIC_PAGE_TYPE_ARTIST" => Some(Hit::Artist(item("artist", if kind_word == "Artist" { rest } else { subtitle.clone() }, None))),
            "MUSIC_PAGE_TYPE_ALBUM" => Some(Hit::Album(item("album", subtitle.clone(), None))),
            "MUSIC_PAGE_TYPE_PLAYLIST" => {
                let pid = r.browse_id.trim_start_matches("VL").to_string();
                Some(Hit::Playlist(item("playlist", subtitle.clone(), Some(pid))))
            }
            _ => None, // profiles, podcasts
        };
    }
    let track = parse_responsive(item)?;
    match kind_word.as_str() {
        "Video" => Some(Hit::Video(track)),
        "Episode" | "Podcast" => None,
        _ => Some(Hit::Song(track)), // "Song", or no prefix in filtered results
    }
}

/// YouTube Music's search page. The "top result" card comes first; every other result is a row
/// (in its own section in the unfiltered layout, in shelves in filtered ones).
fn parse_search(v: &Value) -> SearchResults {
    let mut out = SearchResults::default();
    if let Some(card) = find_first(v, "musicCardShelfRenderer") {
        let run = &card["title"]["runs"][0];
        let nav = &run["navigationEndpoint"];
        let title = text(&card["title"]);
        let subtitle = text(&card["subtitle"]);
        let thumbnail = last_thumbnail(&card["thumbnail"]);
        let base = HomeItem { kind: String::new(), title: title.clone(), subtitle: subtitle.clone(), thumbnail: thumbnail.clone(), video_id: None, playlist_id: None, browse_id: None, track: None };
        out.top = if let Some(r) = browse_ref(nav) {
            let kind = match nav_page_type(nav) {
                Some("MUSIC_PAGE_TYPE_ARTIST") => "artist",
                Some("MUSIC_PAGE_TYPE_ALBUM") => "album",
                Some("MUSIC_PAGE_TYPE_PLAYLIST") => "playlist",
                _ => "",
            };
            (!kind.is_empty()).then(|| HomeItem {
                kind: kind.into(),
                subtitle: subtitle.trim_start_matches("Artist • ").to_string(),
                playlist_id: (kind == "playlist").then(|| r.browse_id.trim_start_matches("VL").to_string()),
                browse_id: Some(r.browse_id),
                ..base.clone()
            })
        } else if let Some(vid) = nav["watchEndpoint"]["videoId"].as_str() {
            let runs: Vec<&Value> = card["subtitle"]["runs"].as_array().map(|r| r.iter().collect()).unwrap_or_default();
            let (artists, album, duration) = split_byline(&runs);
            let t = Track { video_id: vid.into(), title: title.clone(), artist: artists.join(", "), album, duration, thumbnail: thumbnail.clone() };
            Some(HomeItem { kind: "song".into(), video_id: Some(vid.into()), track: Some(t), ..base })
        } else {
            None
        };
    }
    let mut rows = Vec::new();
    find_all(v, "musicResponsiveListItemRenderer", &mut rows);
    for r in rows {
        match classify_row(r) {
            Some(Hit::Song(t)) => out.songs.push(t),
            Some(Hit::Video(t)) => out.videos.push(t),
            Some(Hit::Artist(a)) => out.artists.push(a),
            Some(Hit::Album(a)) => out.albums.push(a),
            Some(Hit::Playlist(p)) => out.playlists.push(p),
            None => {}
        }
    }
    // Songs inside an artist's top-result card leave out the artist's name.
    if let Some(top) = out.top.as_ref().filter(|t| t.kind == "artist") {
        for t in out.songs.iter_mut().filter(|t| t.artist.is_empty()) {
            t.artist = top.title.clone();
        }
    }
    dedupe(&mut out.songs);
    dedupe(&mut out.videos);
    // The top result shouldn't be listed again below it.
    if let Some(top_id) = out.top.as_ref().and_then(|t| t.browse_id.clone()) {
        out.artists.retain(|a| a.browse_id.as_deref() != Some(&top_id));
        out.albums.retain(|a| a.browse_id.as_deref() != Some(&top_id));
    }
    out
}

fn parse_artist(v: &Value, id: &str) -> Option<ArtistPage> {
    let h = find_first(&v["header"], "musicImmersiveHeaderRenderer").or_else(|| find_first(&v["header"], "musicVisualHeaderRenderer"))?;
    let name = text(&h["title"]);
    let stats = Some(text(&h["monthlyListenerCount"]))
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let subs = text(&h["subscriptionButton"]["subscribeButtonRenderer"]["subscriberCountText"]);
            (!subs.is_empty()).then(|| format!("{subs} subscribers"))
        });
    let radio_playlist_id = h["startRadioButton"]["buttonRenderer"]["navigationEndpoint"]["watchEndpoint"]["playlistId"]
        .as_str()
        .or_else(|| h["startRadioButton"]["buttonRenderer"]["navigationEndpoint"]["watchPlaylistEndpoint"]["playlistId"].as_str())
        .map(String::from);
    let sections = find_first(&v["contents"], "sectionListRenderer").and_then(|s| s["contents"].as_array());
    let mut top_songs = Vec::new();
    let mut songs_playlist_id = None;
    let mut shelves = Vec::new();
    for s in sections.into_iter().flatten() {
        if let Some(shelf) = s.get("musicShelfRenderer") {
            let mut rows = Vec::new();
            find_all(shelf, "musicResponsiveListItemRenderer", &mut rows);
            top_songs.extend(rows.into_iter().filter_map(parse_responsive));
            let more = browse_ref(&shelf["bottomEndpoint"]).or_else(|| browse_ref(&shelf["title"]["runs"][0]["navigationEndpoint"]));
            songs_playlist_id = more.map(|m| m.browse_id.trim_start_matches("VL").to_string());
        } else if let Some(c) = s.get("musicCarouselShelfRenderer") {
            let hh = &c["header"]["musicCarouselShelfBasicHeaderRenderer"];
            let title = text(&hh["title"]);
            let items: Vec<HomeItem> = c["contents"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.get("musicTwoRowItemRenderer").and_then(parse_two_row))
                .collect();
            if items.is_empty() {
                continue;
            }
            // Only discography grids and playlists can be opened here ("Live performances" and
            // "Playlists by" link back into the channel itself).
            let more = browse_ref(&hh["moreContentButton"]["buttonRenderer"]["navigationEndpoint"])
                .or_else(|| browse_ref(&hh["title"]["runs"][0]["navigationEndpoint"]))
                .filter(|m| m.browse_id.starts_with("MPAD") || m.browse_id.starts_with("VL"));
            shelves.push(ArtistShelf { title, items, more });
        }
    }
    dedupe(&mut top_songs);
    Some(ArtistPage {
        id: id.to_string(),
        name,
        thumbnail: last_thumbnail(&h["thumbnail"]),
        description: Some(text(&h["description"])).filter(|d| !d.is_empty()),
        stats,
        radio_playlist_id,
        top_songs,
        songs_playlist_id,
        shelves,
    })
}

/// A playlist's or album's name, owner/artist and cover, from its page header.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct HeaderInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub thumbnail: Option<String>,
}

/// A page of the home feed and the token for the next one (None at the end of the feed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomePage {
    pub shelves: Vec<Shelf>,
    /// Opaque to the client: `visitor|token`, since the token only works with the visitor id
    /// of the session that got it.
    pub continuation: Option<String>,
}

/// Podcasts get new episodes daily.
const PODCAST_TTL: i64 = cache::HOUR;

/// A page of a list of tracks (a podcast's episodes) and the token for the next one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackPage {
    pub tracks: Vec<Track>,
    pub continuation: Option<String>,
}

fn track_page(v: &Value) -> TrackPage {
    let mut tracks = parse_responsive_items(v);
    dedupe(&mut tracks);
    TrackPage { tracks, continuation: list_continuation(v) }
}

/// The next-page token of a playlist's (or album's) track list. Not just the first token on the
/// page: that can be the one for the suggestions under the list, which pages nothing in.
fn list_continuation(v: &Value) -> Option<String> {
    if let Some(c) = find_first(v, "continuationItemRenderer") {
        if let Some(t) = c["continuationEndpoint"]["continuationCommand"]["token"].as_str() {
            return Some(t.to_string());
        }
    }
    ["musicPlaylistShelfRenderer", "musicPlaylistShelfContinuation", "musicShelfRenderer", "musicShelfContinuation"]
        .into_iter()
        .filter_map(|k| find_first(v, k))
        .find_map(|s| s["continuations"][0]["nextContinuationData"]["continuation"].as_str().map(String::from))
}

/// The browse id of a playlist or album page.
fn playlist_browse_id(id: &str) -> String {
    if id.starts_with("VL") || id.starts_with("MPRE") || id.starts_with("FE") {
        id.to_string()
    } else {
        format!("VL{id}")
    }
}

fn home_continuation(v: &Value, visitor: Option<&str>) -> Option<String> {
    feed_continuation(v).map(|t| format!("{}|{t}", visitor.unwrap_or_default()))
}

/// The feed's own next-page token (not one of a shelf inside it): on the first page it's on the
/// section list, on later pages on the section list continuation.
fn feed_continuation(v: &Value) -> Option<String> {
    let list = find_first(v, "sectionListContinuation").or_else(|| find_first(v, "sectionListRenderer"))?;
    list["continuations"][0]["nextContinuationData"]["continuation"].as_str().map(String::from)
}

fn header_info(v: &Value) -> HeaderInfo {
    for key in ["musicResponsiveHeaderRenderer", "musicDetailHeaderRenderer", "musicImmersiveHeaderRenderer"] {
        if let Some(h) = find_first(v, key) {
            let artist = h["straplineTextOne"]["runs"]
                .as_array()
                .or_else(|| h["subtitle"]["runs"].as_array())
                .and_then(|runs| {
                    runs.iter()
                        .find(|r| matches!(page_type(r), Some("MUSIC_PAGE_TYPE_ARTIST") | Some("MUSIC_PAGE_TYPE_USER_CHANNEL")))
                        .or_else(|| runs.first())
                        .and_then(|r| r["text"].as_str())
                        .map(String::from)
                });
            return HeaderInfo {
                title: Some(text(&h["title"])).filter(|s| !s.is_empty()),
                artist,
                thumbnail: last_thumbnail(&h["thumbnail"]),
            };
        }
    }
    HeaderInfo::default()
}

pub fn dedupe(tracks: &mut Vec<Track>) {
    let mut seen = std::collections::HashSet::new();
    tracks.retain(|t| seen.insert(t.video_id.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_browse_ids() {
        assert_eq!(playlist_browse_id("PLabc"), "VLPLabc");
        assert_eq!(playlist_browse_id("MPREb_x"), "MPREb_x");
        assert_eq!(playlist_browse_id("VLPLabc"), "VLPLabc");
    }

    #[test]
    fn civil() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_397), (2025, 11, 5));
    }

    #[test]
    fn search_rows_by_kind() {
        let nav = |pt: &str, id: &str| json!({"browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": pt}}}});
        let row = |title: &str, sub: &str, nav: Value, vid: Option<&str>| {
            let mut r = json!({"musicResponsiveListItemRenderer": {"flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": title}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": sub}]}}}
            ], "navigationEndpoint": nav}});
            if let Some(v) = vid {
                r["musicResponsiveListItemRenderer"]["playlistItemData"] = json!({"videoId": v});
            }
            json!({"itemSectionRenderer": {"contents": [r]}})
        };
        let v = json!({"contents": [
            {"musicCardShelfRenderer": {"title": {"runs": [{"text": "Band", "navigationEndpoint": nav("MUSIC_PAGE_TYPE_ARTIST", "UC1")}]},
                "subtitle": {"runs": [{"text": "Artist • 1M monthly audience"}]}}},
            row("Record", "Album • Band • 1997", nav("MUSIC_PAGE_TYPE_ALBUM", "MPRE1"), None),
            row("Other", "Artist • 5K subscribers", nav("MUSIC_PAGE_TYPE_ARTIST", "UC2"), None),
            row("Band", "Artist • 1M monthly audience", nav("MUSIC_PAGE_TYPE_ARTIST", "UC1"), None),
            row("Fan", "Profile • @fan", nav("MUSIC_PAGE_TYPE_USER_CHANNEL", "UC3"), None),
            row("Hit", "Song • Band", json!({}), Some("v1")),
            row("Hit (Live)", "Video • Band • 1M views", json!({}), Some("v2")),
            row("Talk", "Episode • Pod", json!({}), Some("v3")),
            row("Mix", "Playlist • Someone", nav("MUSIC_PAGE_TYPE_PLAYLIST", "VLPL1"), None),
        ]});
        let s = parse_search(&v);
        let top = s.top.unwrap();
        assert_eq!((top.kind.as_str(), top.browse_id.as_deref(), top.subtitle.as_str()), ("artist", Some("UC1"), "1M monthly audience"));
        assert_eq!(s.artists.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Other"]); // top result not repeated, profile skipped
        assert_eq!(s.albums[0].browse_id.as_deref(), Some("MPRE1"));
        assert_eq!(s.songs.iter().map(|t| t.video_id.as_str()).collect::<Vec<_>>(), ["v1"]);
        assert_eq!(s.videos[0].video_id, "v2");
        assert_eq!(s.playlists[0].playlist_id.as_deref(), Some("PL1"));
    }

    #[test]
    fn parses_panel() {
        let v = json!({"contents": [{"playlistPanelVideoRenderer": {
            "videoId": "abc",
            "title": {"runs": [{"text": "Song"}]},
            "lengthText": {"runs": [{"text": "3:10"}]},
            "longBylineText": {"runs": [
                {"text": "Artist", "navigationEndpoint": {"browseEndpoint": {"browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}},
                {"text": " • "},
                {"text": "Album", "navigationEndpoint": {"browseEndpoint": {"browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ALBUM"}}}}},
                {"text": " • "},
                {"text": "2020"}
            ]},
            "thumbnail": {"thumbnails": [{"url": "a"}, {"url": "b"}]}
        }}]});
        let t = parse_panel_items(&v);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].artist, "Artist");
        assert_eq!(t[0].album.as_deref(), Some("Album"));
        assert_eq!(t[0].duration, Some(190));
        assert_eq!(t[0].thumbnail.as_deref(), Some("b"));
    }

    #[test]
    fn header_from_browser_jar() {
        use crate::services::ytdlp::parse_netscape_cookies;
        let jar = "# Netscape HTTP Cookie File\n\
.youtube.com\tTRUE\t/\tTRUE\t1893456000\tSAPISID\ta\n\
#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1893456000\tSID\tb\n\
music.youtube.com\tFALSE\t/\tTRUE\t1893456000\tSID\tc\n\
.google.com\tTRUE\t/\tTRUE\t1893456000\tSAPISID\tignored\n";
        let h = cookie_header(&parse_netscape_cookies(jar)).unwrap();
        assert!(h.contains("SAPISID=a"));
        assert!(h.contains("SID=c")); // the music.youtube.com cookie wins over .youtube.com
        assert!(!h.contains("ignored"));
        assert!(cookie_header(&parse_netscape_cookies(".example.com\tTRUE\t/\tFALSE\t0\tx\ty\n")).is_none());
    }

    #[test]
    fn sapisid_from_cookie() {
        let c = "a=1; SAPISID=xyz; __Secure-3PAPISID=abc";
        assert_eq!(sapisid(c).as_deref(), Some("abc"));
    }
}
