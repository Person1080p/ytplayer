//! Jellyfin as an audio source: sign-in, search, track matching (like Plex), stream URLs, artwork.
//!
//! Songs only in Jellyfin use `jf_<itemId>` ids (Jellyfin item ids: 32 hex characters). Matches
//! of other songs to Jellyfin items are cached in `source_matches` with source 'jellyfin'.

use std::time::Duration;

use anyhow::{anyhow, bail};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    db::Db,
    models::{now, Track},
    services::{
        matching,
        plex::{plex_bitrate, MatchMode},
    },
};

/// Automatic matches below this score aren't used (same scale as Plex's).
pub const MATCH_THRESHOLD: f32 = 0.82;

pub const ID_PREFIX: &str = "jf_";

/// `source` value in `source_matches`.
const SOURCE: &str = "jellyfin";
const CLIENT: &str = "ytp";
const DEVICE: &str = "ytp server";
const NEGATIVE_TTL: i64 = 60 * 60 * 24 * 3;
/// Bumped when automatic matching improves: older automatic matches are redone on next play.
const MATCHER_VERSION: i64 = 1;
/// How many search results automatic matching looks through (live copies can push the studio
/// one well down the list).
const MATCH_SEARCH_LIMIT: usize = 50;
const TIMEOUT: Duration = Duration::from_secs(8);
const ART_TIMEOUT: Duration = Duration::from_secs(15);
/// Settings that make up a connection (the device id stays, so the server sees one device).
const CONNECTION_KEYS: [&str; 5] = ["jellyfin.url", "jellyfin.token", "jellyfin.user_id", "jellyfin.user", "jellyfin.server_name"];
const NOT_JELLYFIN: &str = "That address doesn't look like a Jellyfin server";

/// The Jellyfin item id in a Jellyfin-only id.
pub fn item_id(video_id: &str) -> Option<&str> {
    video_id.strip_prefix(ID_PREFIX).filter(|k| k.len() == 32 && k.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn is_jellyfin_id(video_id: &str) -> bool {
    item_id(video_id).is_some()
}

/// An item id as used in `jf_` ids: 32 lowercase hex characters. Some servers write GUIDs with
/// dashes; those are accepted too.
pub fn normalize_item_id(id: &str) -> Option<String> {
    let id: String = id.trim().chars().filter(|&c| c != '-').collect::<String>().to_ascii_lowercase();
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
}

/// What a song plays from in Jellyfin.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JellyfinTrack {
    pub item_id: String,
    pub codec: Option<String>,
    pub title: String,
    pub artist: String,
}

/// A Jellyfin track with what matching and display need.
#[derive(Debug, Clone)]
pub struct JellyfinHit {
    pub track: JellyfinTrack,
    pub duration: Option<u32>,
    pub album_artist: String,
    pub album: Option<String>,
    pub has_art: bool,
}

/// Containers browsers play as they are.
const FRIENDLY_CONTAINERS: [&str; 11] = ["mp3", "m4a", "mp4", "m4b", "aac", "flac", "ogg", "oga", "opus", "webm", "wav"];

/// The codec to store for an item: the audio stream's codec, or the container when that's one
/// browsers can't open (so the file gets transcoded whatever is inside it).
fn pick_codec(container: Option<&str>, stream_codec: Option<&str>) -> Option<String> {
    let container = container.map(str::to_ascii_lowercase).filter(|c| !c.is_empty());
    let stream_codec = stream_codec.map(str::to_ascii_lowercase).filter(|c| !c.is_empty());
    // Jellyfin sometimes reports a list ("mov,mp4,m4a,3gp,3g2,mj2").
    let friendly = container.as_deref().map_or(true, |c| c.split(',').any(|p| FRIENDLY_CONTAINERS.contains(&p.trim())));
    if friendly {
        stream_codec.or(container)
    } else {
        container
    }
}

impl JellyfinHit {
    /// From an item's JSON (`BaseItemDto`). None for anything that isn't a usable item.
    fn parse(m: &Value) -> Option<Self> {
        let item_id = normalize_item_id(m["Id"].as_str()?)?;
        let str_of = |k: &str| m[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let album_artist = str_of("AlbumArtist").unwrap_or_default();
        let artists: Vec<&str> =
            m["Artists"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).map(str::trim).filter(|a| !a.is_empty()).collect();
        let artist = if artists.is_empty() { album_artist.clone() } else { artists.join(", ") };
        let source = &m["MediaSources"][0];
        let stream_codec = source["MediaStreams"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["Type"].as_str() == Some("Audio"))
            .and_then(|s| s["Codec"].as_str());
        let container = source["Container"].as_str().or_else(|| m["Container"].as_str());
        let ticks = m["RunTimeTicks"].as_u64().or_else(|| source["RunTimeTicks"].as_u64());
        let has_art = m["ImageTags"]["Primary"].as_str().is_some_and(|s| !s.is_empty())
            || m["AlbumPrimaryImageTag"].as_str().is_some_and(|s| !s.is_empty());
        Some(Self {
            track: JellyfinTrack {
                item_id,
                codec: pick_codec(container, stream_codec),
                title: str_of("Name").unwrap_or_default(),
                artist,
            },
            // Run time is in 100 ns ticks.
            duration: ticks.map(|t| (t / 10_000_000) as u32).filter(|&d| d > 0),
            album_artist,
            album: str_of("Album"),
            has_art,
        })
    }

    /// As a song in the app, under its Jellyfin-only id (art through `/api/jellyfin/art`).
    pub fn to_track(&self) -> Track {
        Track {
            video_id: format!("{ID_PREFIX}{}", self.track.item_id),
            title: self.track.title.clone(),
            artist: self.track.artist.clone(),
            album: self.album.clone(),
            duration: self.duration,
            thumbnail: self.has_art.then(|| format!("/api/jellyfin/art?id={}", self.track.item_id)),
        }
    }
}

/// How well a Jellyfin track matches `want`: (score, checked against the match threshold; rank,
/// which also prefers the same album and the studio recording), as Plex's [`crate::services::plex::rank`].
pub fn rank(want: &Track, h: &JellyfinHit) -> (f32, f32) {
    let jt = &h.track;
    let s = |artist: &str| matching::score_unclamped(&want.title, &want.artist, want.duration, &jt.title, artist, h.duration);
    let base = s(&jt.artist).max(s(&h.album_artist));
    let mut r = base;
    if let (Some(a), Some(b)) = (want.album.as_deref(), h.album.as_deref()) {
        r += 0.15 * matching::similarity(&matching::normalize(a), &matching::normalize(b));
    }
    if matching::looks_live(&jt.title, h.album.as_deref()) && !matching::looks_live(&want.title, want.album.as_deref()) {
        r -= 0.2;
    }
    (base.clamp(0.0, 1.0), r)
}

/// Connection details shown in Settings.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JellyfinInfo {
    pub connected: bool,
    pub url: Option<String>,
    pub server_name: Option<String>,
    pub user: Option<String>,
}

/// A signed-in server, as stored.
struct Conn {
    url: String,
    token: String,
    user_id: String,
}

/// What a song plays from in Jellyfin, as stored.
struct StoredMatch {
    track: Option<JellyfinTrack>,
    checked_at: i64,
    /// Chosen by you (an item, or "don't use Jellyfin") rather than by automatic matching.
    manual: bool,
    version: i64,
}

/// The `Authorization: MediaBrowser …` value Jellyfin identifies clients by.
fn auth_value(device_id: &str, token: Option<&str>) -> String {
    let mut v = format!(
        r#"MediaBrowser Client="{CLIENT}", Device="{DEVICE}", DeviceId="{device_id}", Version="{}""#,
        env!("CARGO_PKG_VERSION")
    );
    if let Some(t) = token {
        v += &format!(r#", Token="{t}""#);
    }
    v
}

/// A server address as typed ("jf.local:8096", "https://host/jellyfin/") → base URL without a
/// trailing slash.
fn normalize_url(input: &str) -> anyhow::Result<String> {
    let s = input.trim();
    if s.is_empty() {
        bail!("Enter the Jellyfin server address");
    }
    let s = if s.contains("://") { s.to_string() } else { format!("http://{s}") };
    let mut u = reqwest::Url::parse(&s).map_err(|_| anyhow!("That isn't a valid server address"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        bail!("That isn't a valid server address");
    }
    u.set_query(None);
    u.set_fragment(None);
    Ok(u.as_str().trim_end_matches('/').to_string())
}

/// Codecs browsers play from the original file.
fn browser_friendly(codec: Option<&str>) -> bool {
    matches!(
        codec,
        None | Some("mp3" | "aac" | "m4a" | "flac" | "opus" | "vorbis" | "ogg" | "oga" | "wav" | "pcm_s16le" | "pcm_s24le")
    )
}

fn lossless(codec: Option<&str>) -> bool {
    matches!(codec, Some(c) if matches!(c, "flac" | "alac" | "wav" | "ape" | "wv" | "wavpack" | "aiff") || c.starts_with("pcm") || c.starts_with("dsd"))
}

/// The audio URL for an item and its approximate bitrate: the original file when browsers play
/// it and it fits `budget_kbps`, else an MP3 transcode that does.
fn build_stream_url(base: &str, token: &str, device_id: &str, item_id: &str, codec: Option<&str>, budget_kbps: Option<u32>) -> (String, u32) {
    let codec = codec.map(str::to_ascii_lowercase);
    // The match cache doesn't keep the file's bitrate: assume typical values per codec.
    let original = if lossless(codec.as_deref()) { 1000 } else { 320 };
    let token = urlencoding::encode(token);
    if browser_friendly(codec.as_deref()) && budget_kbps.map_or(true, |b| b >= original) {
        return (format!("{base}/Audio/{item_id}/stream?static=true&api_key={token}"), original);
    }
    let rate = plex_bitrate(budget_kbps);
    (
        format!(
            "{base}/Audio/{item_id}/stream.mp3?audioCodec=mp3&audioBitRate={}&deviceId={}&api_key={token}",
            rate * 1000,
            urlencoding::encode(device_id)
        ),
        rate,
    )
}

/// A request error as plain words for the sign-in screen.
fn unreachable(url: &str, e: &reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        anyhow!("Couldn't reach a server at {url} (it didn't answer in time)")
    } else {
        anyhow!("Couldn't reach a server at {url}")
    }
}

#[derive(Clone)]
pub struct Jellyfin {
    http: reqwest::Client,
    db: Db,
}

impl Jellyfin {
    pub fn new(http: reqwest::Client, db: Db) -> Self {
        Self { http, db }
    }

    /// This server's stable device id (Jellyfin keeps one session per device).
    async fn device_id(&self) -> anyhow::Result<String> {
        if let Some(id) = self.db.get_setting("jellyfin.device_id").await? {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.db.set_setting("jellyfin.device_id", &id).await?;
        Ok(id)
    }

    async fn setting(&self, key: &str) -> Option<String> {
        self.db.get_setting(key).await.ok().flatten().filter(|s| !s.is_empty())
    }

    async fn conn(&self) -> Option<Conn> {
        Some(Conn {
            url: self.setting("jellyfin.url").await?,
            token: self.setting("jellyfin.token").await?,
            user_id: self.setting("jellyfin.user_id").await.unwrap_or_default(),
        })
    }

    async fn require_conn(&self) -> anyhow::Result<Conn> {
        self.conn().await.ok_or_else(|| anyhow!("Jellyfin isn't connected"))
    }

    /// A signed request to the server.
    async fn request(&self, c: &Conn, method: reqwest::Method, path: &str, timeout: Duration) -> anyhow::Result<reqwest::RequestBuilder> {
        let auth = auth_value(&self.device_id().await?, Some(&c.token));
        Ok(self
            .http
            .request(method, format!("{}{path}", c.url))
            .header("accept", "application/json")
            .header("Authorization", &auth)
            .header("X-Emby-Authorization", &auth)
            .header("X-Emby-Token", &c.token)
            .timeout(timeout))
    }

    async fn get(&self, c: &Conn, path: &str, timeout: Duration) -> anyhow::Result<reqwest::RequestBuilder> {
        self.request(c, reqwest::Method::GET, path, timeout).await
    }

    /// A server is connected (signed in).
    pub async fn configured(&self) -> bool {
        self.conn().await.is_some()
    }

    pub async fn info(&self) -> JellyfinInfo {
        JellyfinInfo {
            connected: self.configured().await,
            url: self.setting("jellyfin.url").await,
            server_name: self.setting("jellyfin.server_name").await,
            user: self.setting("jellyfin.user").await,
        }
    }

    /// Sign in to a server with a Jellyfin user; stores the access token. Returns the info.
    pub async fn connect(&self, url: &str, username: &str, password: &str) -> anyhow::Result<JellyfinInfo> {
        let url = normalize_url(url)?;
        let username = username.trim();
        if username.is_empty() {
            bail!("Enter your Jellyfin user name");
        }
        let resp = self
            .http
            .get(format!("{url}/System/Info/Public"))
            .header("accept", "application/json")
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| unreachable(&url, &e))?;
        if !resp.status().is_success() {
            bail!(NOT_JELLYFIN);
        }
        let public: Value = resp.json().await.map_err(|_| anyhow!(NOT_JELLYFIN))?;
        if public["Id"].as_str().is_none() {
            bail!(NOT_JELLYFIN);
        }
        let server_name = public["ServerName"].as_str().filter(|s| !s.is_empty()).unwrap_or("Jellyfin").to_string();

        let auth = auth_value(&self.device_id().await?, None);
        let resp = self
            .http
            .post(format!("{url}/Users/AuthenticateByName"))
            .header("accept", "application/json")
            .header("Authorization", &auth)
            .header("X-Emby-Authorization", &auth)
            .json(&json!({ "Username": username, "Pw": password }))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| unreachable(&url, &e))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            bail!("Wrong user name or password");
        }
        if !status.is_success() {
            bail!("The Jellyfin server refused the sign-in (HTTP {})", status.as_u16());
        }
        let v: Value = resp.json().await.map_err(|_| anyhow!(NOT_JELLYFIN))?;
        let token = v["AccessToken"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!(NOT_JELLYFIN))?;
        let user_id = v["User"]["Id"].as_str().and_then(normalize_item_id).ok_or_else(|| anyhow!(NOT_JELLYFIN))?;
        let user = v["User"]["Name"].as_str().unwrap_or(username);

        self.db.set_setting("jellyfin.url", &url).await?;
        self.db.set_setting("jellyfin.token", token).await?;
        self.db.set_setting("jellyfin.user_id", &user_id).await?;
        self.db.set_setting("jellyfin.user", user).await?;
        self.db.set_setting("jellyfin.server_name", &server_name).await?;
        // Matches against another server (or user) don't hold.
        self.clear_cache().await?;
        Ok(self.info().await)
    }

    pub async fn disconnect(&self) -> anyhow::Result<()> {
        // Sign the token out on the server too, if it's reachable.
        if let Some(c) = self.conn().await {
            if let Ok(req) = self.request(&c, reqwest::Method::POST, "/Sessions/Logout", Duration::from_secs(3)).await {
                let _ = req.send().await;
            }
        }
        for k in CONNECTION_KEYS {
            let k = k.to_string();
            self.db.call(move |c| c.execute("DELETE FROM settings WHERE key=?1", [k]).map(|_| ())).await?;
        }
        self.clear_cache().await
    }

    async fn clear_cache(&self) -> anyhow::Result<()> {
        self.db.call(|c| c.execute("DELETE FROM source_matches WHERE source=?1", [SOURCE]).map(|_| ())).await
    }

    // ---- items ------------------------------------------------------------

    /// Audio items for a query (`/Items`, or `/Users/{id}/Items` on servers older than 10.9).
    async fn items(&self, query: &[(&str, &str)]) -> anyhow::Result<Vec<JellyfinHit>> {
        let c = self.require_conn().await?;
        let mut q: Vec<(&str, &str)> =
            vec![("IncludeItemTypes", "Audio"), ("Recursive", "true"), ("Fields", "MediaSources,Path")];
        q.extend_from_slice(query);
        let mut with_user = q.clone();
        with_user.push(("userId", &c.user_id));
        let mut resp = self.get(&c, "/Items", TIMEOUT).await?.query(&with_user).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let path = format!("/Users/{}/Items", c.user_id);
            resp = self.get(&c, &path, TIMEOUT).await?.query(&q).send().await?;
        }
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Jellyfin sign-in expired: sign in again in Settings");
        }
        let v: Value = resp.error_for_status()?.json().await?;
        Ok(v["Items"].as_array().into_iter().flatten().filter_map(JellyfinHit::parse).collect())
    }

    /// Audio tracks matching a search.
    pub async fn search(&self, query: &str) -> anyhow::Result<Vec<JellyfinHit>> {
        self.search_n(query, 25).await
    }

    /// Up to `limit` audio tracks matching a search.
    pub async fn search_n(&self, query: &str, limit: usize) -> anyhow::Result<Vec<JellyfinHit>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.to_string();
        self.items(&[("searchTerm", query), ("Limit", &limit)]).await
    }

    /// A track by item id (None when the server no longer has it).
    pub async fn track_by_id(&self, item_id: &str) -> anyhow::Result<Option<JellyfinHit>> {
        let Some(id) = normalize_item_id(item_id) else { return Ok(None) };
        Ok(self.items(&[("Ids", &id)]).await?.into_iter().next())
    }

    /// The track behind a Jellyfin-only id (`jf_<itemId>`), cached like matches.
    pub async fn track_for_id(&self, video_id: &str) -> anyhow::Result<Option<JellyfinTrack>> {
        let Some(id) = item_id(video_id) else { return Ok(None) };
        if let Some(StoredMatch { track: Some(t), .. }) = self.cached(video_id).await? {
            return Ok(Some(t));
        }
        let found = self.track_by_id(id).await?.map(|h| h.track);
        if found.is_some() {
            self.store(video_id, found.as_ref(), true).await?;
        }
        Ok(found)
    }

    // ---- matching ---------------------------------------------------------

    /// Jellyfin tracks that could be `track`, best first, with their match score.
    pub async fn candidates(&self, track: &Track) -> anyhow::Result<Vec<(JellyfinHit, f32)>> {
        let title_q = matching::normalize(&track.title);
        let mut hits = self.search_n(&title_q, MATCH_SEARCH_LIMIT).await?;
        // Jellyfin searches names as written, so punctuation can matter ("Don't" vs "dont").
        if hits.is_empty() && track.title.trim().to_lowercase() != title_q {
            hits = self.search_n(&track.title, MATCH_SEARCH_LIMIT).await?;
        }
        if hits.is_empty() {
            hits = self.search_n(&format!("{} {}", matching::primary_artist(&track.artist), title_q), MATCH_SEARCH_LIMIT).await?;
        }
        let mut ranked: Vec<(JellyfinHit, f32, f32)> = hits
            .into_iter()
            .map(|h| {
                let (score, r) = rank(track, &h);
                (h, score, r)
            })
            .collect();
        ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
        Ok(ranked.into_iter().map(|(h, s, _)| (h, s)).collect())
    }

    /// The Jellyfin copy of a song (any id: YouTube, Spotify, imported, Plex-only, local-only),
    /// cached in `source_matches`. A track you picked (or "don't use Jellyfin") always wins.
    pub async fn find_match(&self, track: &Track) -> anyhow::Result<Option<JellyfinTrack>> {
        if is_jellyfin_id(&track.video_id) {
            return self.track_for_id(&track.video_id).await;
        }
        if !self.configured().await {
            return Ok(None);
        }
        if let Some(m) = self.cached(&track.video_id).await? {
            if m.manual || (m.track.is_some() && m.version >= MATCHER_VERSION) {
                return Ok(m.track);
            }
            if m.track.is_none() && m.version >= MATCHER_VERSION && now() - m.checked_at < NEGATIVE_TTL {
                return Ok(None);
            }
        }
        let result = self
            .candidates(track)
            .await?
            .into_iter()
            .find(|(_, score)| *score >= MATCH_THRESHOLD)
            .map(|(h, _)| h.track);
        self.store(&track.video_id, result.as_ref(), false).await?;
        Ok(result)
    }

    /// How a song's Jellyfin copy is chosen now, and the stored track if any.
    pub async fn choice(&self, video_id: &str) -> anyhow::Result<(MatchMode, Option<JellyfinTrack>)> {
        Ok(match self.cached(video_id).await? {
            Some(m) if m.manual => (if m.track.is_some() { MatchMode::Picked } else { MatchMode::Off }, m.track),
            // An automatic match from an older matcher is redone on next play.
            Some(m) => (MatchMode::Auto, m.track.filter(|_| m.version >= MATCHER_VERSION)),
            None => (MatchMode::Auto, None),
        })
    }

    /// Play `video_id` from this Jellyfin item, whatever automatic matching thinks.
    pub async fn pin(&self, video_id: &str, item_id: &str) -> anyhow::Result<JellyfinTrack> {
        if normalize_item_id(item_id).is_none() {
            bail!("not a Jellyfin item id");
        }
        let Some(hit) = self.track_by_id(item_id).await? else { bail!("That track is no longer on the Jellyfin server") };
        self.store(video_id, Some(&hit.track), true).await?;
        Ok(hit.track)
    }

    /// Never play `video_id` from Jellyfin (until you pick a track or go back to automatic).
    pub async fn block(&self, video_id: &str) -> anyhow::Result<()> {
        self.store(video_id, None, true).await
    }

    /// Back to automatic matching for `video_id`: it's matched afresh on next play.
    pub async fn forget(&self, video_id: &str) -> anyhow::Result<()> {
        let vid = video_id.to_string();
        self.db
            .call(move |c| c.execute("DELETE FROM source_matches WHERE source=?1 AND video_id=?2", params![SOURCE, vid]).map(|_| ()))
            .await
    }

    /// The stored match for `video_id`, if any.
    async fn cached(&self, video_id: &str) -> anyhow::Result<Option<StoredMatch>> {
        let vid = video_id.to_string();
        self.db
            .call(move |c| {
                c.query_row(
                    "SELECT matched, item_id, codec, title, artist, checked_at, manual, version
                     FROM source_matches WHERE source=?1 AND video_id=?2",
                    params![SOURCE, vid],
                    |r| {
                        let matched: bool = r.get(0)?;
                        let item: Option<String> = r.get(1)?;
                        let track = match item {
                            Some(item_id) if matched => Some(JellyfinTrack {
                                item_id,
                                codec: r.get(2)?,
                                title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                                artist: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                            }),
                            _ => None,
                        };
                        Ok(StoredMatch { track, checked_at: r.get(5)?, manual: r.get(6)?, version: r.get(7)? })
                    },
                )
                .optional()
            })
            .await
    }

    /// Save a match (or that there is none) for `video_id`.
    async fn store(&self, video_id: &str, track: Option<&JellyfinTrack>, manual: bool) -> anyhow::Result<()> {
        let vid = video_id.to_string();
        let t = track.cloned();
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO source_matches(source,video_id,matched,item_id,codec,title,artist,checked_at,manual,version)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        SOURCE,
                        vid,
                        t.is_some(),
                        t.as_ref().map(|t| &t.item_id),
                        t.as_ref().and_then(|t| t.codec.clone()),
                        t.as_ref().map(|t| &t.title),
                        t.as_ref().map(|t| &t.artist),
                        now(),
                        manual,
                        MATCHER_VERSION
                    ],
                )
                .map(|_| ())
            })
            .await
    }

    // ---- audio and artwork --------------------------------------------------

    /// URL the server fetches the audio from (proxied to the browser), and its approximate
    /// bitrate. Within `budget_kbps` (transcoded to MP3 when the original is bigger or the codec
    /// isn't one browsers play).
    pub async fn stream_url(&self, t: &JellyfinTrack, budget_kbps: Option<u32>) -> anyhow::Result<(String, u32)> {
        let c = self.require_conn().await?;
        let Some(id) = normalize_item_id(&t.item_id) else { bail!("not a Jellyfin item id") };
        let device = self.device_id().await?;
        Ok(build_stream_url(&c.url, &c.token, &device, &id, t.codec.as_deref(), budget_kbps))
    }

    /// Artwork of an item, resized (the album's when the track has none of its own).
    pub async fn art(&self, item_id: &str) -> anyhow::Result<Vec<u8>> {
        let Some(id) = normalize_item_id(item_id) else { bail!("not a Jellyfin item id") };
        let c = self.require_conn().await?;
        if let Some(bytes) = self.image(&c, &id).await? {
            return Ok(bytes);
        }
        // Tracks often have only album art.
        let req = self.get(&c, &format!("/Items/{id}"), TIMEOUT).await?;
        let mut resp = req.query(&[("userId", c.user_id.as_str())]).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            resp = self.get(&c, &format!("/Users/{}/Items/{id}", c.user_id), TIMEOUT).await?.send().await?;
        }
        let v: Value = resp.error_for_status()?.json().await?;
        if let Some(album) = v["AlbumId"].as_str().and_then(normalize_item_id) {
            if let Some(bytes) = self.image(&c, &album).await? {
                return Ok(bytes);
            }
        }
        bail!("no artwork")
    }

    /// An item's primary image, None when it has none.
    async fn image(&self, c: &Conn, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let resp = self
            .get(c, &format!("/Items/{id}/Images/Primary"), ART_TIMEOUT)
            .await?
            .header("accept", "image/*")
            .query(&[("maxWidth", "400"), ("maxHeight", "400"), ("quality", "90")])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(resp.error_for_status()?.bytes().await?.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> Value {
        json!({
            "Id": "0123456789abcdef0123456789ABCDEF", "Name": "Song", "Type": "Audio",
            "Artists": ["Band", "Guest"], "AlbumArtist": "Band", "Album": "Album",
            "RunTimeTicks": 2_015_000_000u64,
            "ImageTags": { "Primary": "abc" },
            "MediaSources": [{ "Container": "flac", "Bitrate": 900000,
                "MediaStreams": [{ "Type": "Audio", "Codec": "flac" }] }]
        })
    }

    #[test]
    fn ids() {
        assert_eq!(item_id("jf_0123456789abcdef0123456789abcdef"), Some("0123456789abcdef0123456789abcdef"));
        assert!(!is_jellyfin_id("jf_0123"));
        assert!(!is_jellyfin_id("jf_0123456789abcdef0123456789abcdeg"));
        assert!(!is_jellyfin_id("plex_12"));
        assert_eq!(
            normalize_item_id("01234567-89AB-CDEF-0123-456789abcdef").as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(normalize_item_id("../etc"), None);
    }

    #[test]
    fn parses_items() {
        let h = JellyfinHit::parse(&item()).unwrap();
        assert_eq!(h.track.item_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(h.track.artist, "Band, Guest");
        assert_eq!(h.track.codec.as_deref(), Some("flac"));
        let t = h.to_track();
        assert_eq!(t.video_id, "jf_0123456789abcdef0123456789abcdef");
        assert_eq!(t.duration, Some(201));
        assert_eq!(t.album.as_deref(), Some("Album"));
        assert_eq!(t.thumbnail.as_deref(), Some("/api/jellyfin/art?id=0123456789abcdef0123456789abcdef"));

        // No track artists: the album artist. Album art only still counts as art.
        let mut m = item();
        m["Artists"] = json!([]);
        m["ImageTags"] = json!({});
        m["AlbumPrimaryImageTag"] = json!("xyz");
        m["Id"] = json!("01234567-89ab-cdef-0123-456789abcdef");
        let h = JellyfinHit::parse(&m).unwrap();
        assert_eq!(h.track.artist, "Band");
        assert_eq!(h.track.item_id, "0123456789abcdef0123456789abcdef");
        assert!(h.has_art);

        // No art at all, no run time.
        m.as_object_mut().unwrap().remove("AlbumPrimaryImageTag");
        m.as_object_mut().unwrap().remove("RunTimeTicks");
        let t = JellyfinHit::parse(&m).unwrap().to_track();
        assert_eq!(t.thumbnail, None);
        assert_eq!(t.duration, None);

        assert!(JellyfinHit::parse(&json!({ "Id": "nope", "Name": "x" })).is_none());
    }

    #[test]
    fn codecs() {
        assert_eq!(pick_codec(Some("m4a"), Some("alac")).as_deref(), Some("alac"));
        assert_eq!(pick_codec(Some("mov,mp4,m4a,3gp,3g2,mj2"), Some("aac")).as_deref(), Some("aac"));
        assert_eq!(pick_codec(Some("asf"), Some("wmav2")).as_deref(), Some("asf"));
        assert_eq!(pick_codec(Some("mp3"), None).as_deref(), Some("mp3"));
        assert_eq!(pick_codec(None, None), None);
    }

    #[test]
    fn stream_urls() {
        let id = "0123456789abcdef0123456789abcdef";
        let (u, k) = build_stream_url("http://jf:8096", "tok", "dev", id, Some("mp3"), None);
        assert_eq!(u, format!("http://jf:8096/Audio/{id}/stream?static=true&api_key=tok"));
        assert_eq!(k, 320);
        let (u, k) = build_stream_url("http://jf:8096", "tok", "dev", id, Some("flac"), Some(5000));
        assert!(u.contains("stream?static=true"));
        assert_eq!(k, 1000);
        // Lossless over budget: transcoded to fit.
        let (u, k) = build_stream_url("http://jf:8096", "tok", "dev", id, Some("FLAC"), Some(200));
        assert_eq!(u, format!("http://jf:8096/Audio/{id}/stream.mp3?audioCodec=mp3&audioBitRate=192000&deviceId=dev&api_key=tok"));
        assert_eq!(k, 192);
        // Codecs browsers don't play: transcoded even with no budget.
        let (u, k) = build_stream_url("http://jf:8096", "tok", "dev", id, Some("alac"), None);
        assert!(u.contains("stream.mp3?audioCodec=mp3&audioBitRate=320000"));
        assert_eq!(k, 320);
    }

    #[test]
    fn urls() {
        assert_eq!(normalize_url(" jf.local:8096/ ").unwrap(), "http://jf.local:8096");
        assert_eq!(normalize_url("https://host/jellyfin/").unwrap(), "https://host/jellyfin");
        assert!(normalize_url("ftp://host").is_err());
        assert!(normalize_url("").is_err());
    }

    #[test]
    fn auth_header() {
        let v = auth_value("dev", Some("tok"));
        assert!(v.starts_with(r#"MediaBrowser Client="ytp", Device="ytp server", DeviceId="dev", Version=""#));
        assert!(v.ends_with(r#", Token="tok""#));
    }
}

/// Sign-in, matching and streaming against a tiny fake Jellyfin.
#[cfg(test)]
mod flow_tests {
    use axum::{
        extract::{Path, Query},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use std::collections::HashMap;

    use super::*;

    const TOKEN: &str = "tok";
    const STUDIO: &str = "11111111111111111111111111111111";
    const LIVE: &str = "22222222-2222-2222-2222-222222222222";

    fn items() -> Vec<Value> {
        vec![
            json!({ "Id": LIVE, "Name": "Get Lucky (Live)", "Artists": ["Daft Punk"], "Album": "Alive",
                    "RunTimeTicks": 3_000_000_000u64,
                    "MediaSources": [{ "Container": "mp3", "MediaStreams": [{ "Type": "Audio", "Codec": "mp3" }] }] }),
            json!({ "Id": STUDIO, "Name": "Get Lucky", "Artists": ["Daft Punk", "Pharrell Williams"],
                    "Album": "Random Access Memories", "RunTimeTicks": 2_480_000_000u64,
                    "MediaSources": [{ "Container": "flac", "MediaStreams": [{ "Type": "Audio", "Codec": "flac" }] }] }),
        ]
    }

    fn authed(h: &HeaderMap, q: &HashMap<String, String>) -> bool {
        h.get("x-emby-token").is_some_and(|v| v == TOKEN) || q.get("api_key").is_some_and(|v| v == TOKEN)
    }

    async fn fake() -> String {
        let app = Router::new()
            .route("/System/Info/Public", get(|| async { Json(json!({ "Id": "srv", "ServerName": "Home" })) }))
            .route(
                "/Users/AuthenticateByName",
                post(|h: HeaderMap, Json(b): Json<Value>| async move {
                    let client = h.get("authorization").and_then(|v| v.to_str().ok()).is_some_and(|v| v.contains("DeviceId=\""));
                    if client && b["Username"] == "me" && b["Pw"] == "pw" {
                        Ok(Json(json!({ "AccessToken": TOKEN, "User": { "Id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "Name": "Me" } })))
                    } else {
                        Err(StatusCode::UNAUTHORIZED)
                    }
                }),
            )
            .route(
                "/Items",
                get(|h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    if !authed(&h, &q) || q.get("userId").map(String::as_str) != Some("aaaaaaaabbbbccccddddeeeeeeeeeeee") {
                        return Err(StatusCode::UNAUTHORIZED);
                    }
                    let term = q.get("searchTerm").map(|s| s.to_lowercase());
                    let ids = q.get("Ids").cloned();
                    let found: Vec<Value> = items()
                        .into_iter()
                        .filter(|i| term.as_ref().map_or(true, |t| i["Name"].as_str().unwrap().to_lowercase().contains(t)))
                        .filter(|i| ids.as_ref().map_or(true, |id| i["Id"].as_str().unwrap().replace('-', "") == *id))
                        .collect();
                    Ok(Json(json!({ "Items": found })))
                }),
            )
            .route(
                "/Audio/{id}/{file}",
                get(|h: HeaderMap, Path((id, file)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>| async move {
                    if !authed(&h, &q) {
                        return Err(StatusCode::UNAUTHORIZED);
                    }
                    Ok(format!("{file}:{id}"))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("127.0.0.1:{}/", addr.port())
    }

    #[tokio::test]
    async fn connect_match_stream() {
        let url = fake().await;
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        let jf = Jellyfin::new(reqwest::Client::new(), db);
        assert!(!jf.configured().await);

        let err = jf.connect(&url, "me", "nope").await.unwrap_err();
        assert_eq!(err.to_string(), "Wrong user name or password");
        let err = jf.connect("127.0.0.1:1", "me", "pw").await.unwrap_err();
        assert!(err.to_string().starts_with("Couldn't reach"));

        let info = jf.connect(&url, "me", "pw").await.unwrap();
        assert!(info.connected);
        assert_eq!(info.server_name.as_deref(), Some("Home"));
        assert_eq!(info.user.as_deref(), Some("Me"));
        assert!(!info.url.unwrap().ends_with('/'));

        // The studio copy wins over the live one.
        let want = Track {
            video_id: "dQw4w9WgXcQ".into(),
            title: "Get Lucky".into(),
            artist: "Daft Punk".into(),
            album: None,
            duration: Some(248),
            thumbnail: None,
        };
        let m = jf.find_match(&want).await.unwrap().unwrap();
        assert_eq!(m.item_id, STUDIO);
        assert_eq!(jf.choice(&want.video_id).await.unwrap().0, MatchMode::Auto);

        // Direct file, or an MP3 that fits the budget.
        let (u, kbps) = jf.stream_url(&m, None).await.unwrap();
        assert_eq!((kbps, reqwest::get(&u).await.unwrap().text().await.unwrap()), (1000, format!("stream:{STUDIO}")));
        let (u, kbps) = jf.stream_url(&m, Some(128)).await.unwrap();
        assert_eq!((kbps, reqwest::get(&u).await.unwrap().text().await.unwrap()), (128, format!("stream.mp3:{STUDIO}")));

        // Picks and blocks win over automatic matching.
        let live = jf.pin(&want.video_id, LIVE).await.unwrap();
        assert_eq!(live.item_id, "22222222222222222222222222222222");
        assert_eq!(jf.find_match(&want).await.unwrap().unwrap().item_id, live.item_id);
        jf.block(&want.video_id).await.unwrap();
        assert!(jf.find_match(&want).await.unwrap().is_none());
        assert_eq!(jf.choice(&want.video_id).await.unwrap().0, MatchMode::Off);
        jf.forget(&want.video_id).await.unwrap();
        assert_eq!(jf.find_match(&want).await.unwrap().unwrap().item_id, STUDIO);

        // Jellyfin-only ids.
        let own = Track { video_id: format!("jf_{STUDIO}"), ..want.clone() };
        assert_eq!(jf.find_match(&own).await.unwrap().unwrap().title, "Get Lucky");

        jf.disconnect().await.unwrap();
        assert!(!jf.configured().await);
        assert_eq!(jf.choice(&want.video_id).await.unwrap().1.map(|t| t.item_id), None);
        assert!(jf.find_match(&want).await.unwrap().is_none());
    }
}
