//! Spotify playlists imported from a link.
//!
//! The list comes from Spotify itself and is stored, so opening it costs no YouTube calls; after
//! a while it's fetched again so it follows changes made on Spotify. Songs are queued under their
//! own ids (`spotify_<trackId>`) and only matched to audio (Plex, then YouTube) when they play —
//! see [`youtube_id`].
//!
//! The list comes from Spotify's public embed page, which shows the first 100 songs; past that it
//! comes from a playlist export (Chosic's exporter), picked up from the Downloads folder.

use anyhow::{bail, Context};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    models::{now, Track},
    state::AppState,
};

pub const ID_PREFIX: &str = "spotify_";
/// How many songs the embed page lists at most.
pub const EMBED_LIMIT: usize = 100;
/// A stored list older than this is fetched again when it's opened.
pub const RESYNC_AFTER: i64 = 60 * 60;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0";

/// The Spotify track id in a Spotify song id.
pub fn track_id(video_id: &str) -> Option<&str> {
    video_id.strip_prefix(ID_PREFIX).filter(|k| is_base62_id(k))
}

pub fn is_spotify_id(video_id: &str) -> bool {
    track_id(video_id).is_some()
}

fn is_base62_id(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The playlist id in a Spotify link (`open.spotify.com/playlist/…`, also with `intl-xx/`, a
/// `?si=` share suffix or `embed/`), a `spotify:playlist:…` uri, or a bare id.
pub fn playlist_id(input: &str) -> Option<String> {
    let s = input.trim();
    if is_base62_id(s) {
        return Some(s.to_string());
    }
    if let Some(rest) = s.strip_prefix("spotify:playlist:") {
        return is_base62_id(rest).then(|| rest.to_string());
    }
    let after = s.split("/playlist/").nth(1)?;
    let id: String = after.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    is_base62_id(&id).then_some(id)
}

/// A stored Spotify playlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyPlaylist {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub thumbnail: Option<String>,
    pub tracks: Vec<Track>,
    /// "embed" (at most the first 100 songs) or "export" (the first 100 from Spotify, the rest from
    /// a playlist export such as Chosic's).
    pub source: String,
    pub synced_at: i64,
    /// Set when the list may be missing songs, or the last resync failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When an export file (Chosic, Exportify…) gave the list past the first 100 songs (see [`add_export`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifySummary {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub thumbnail: Option<String>,
    pub count: usize,
    pub synced_at: i64,
}

// ---- fetching -------------------------------------------------------------------------

/// The playlist as it is on Spotify now.
async fn fetch(st: &AppState, id: &str) -> anyhow::Result<SpotifyPlaylist> {
    fetch_embed(st, id).await
}

/// From the public embed page (what sites show as a playlist widget): the first 100 songs.
async fn fetch_embed(st: &AppState, id: &str) -> anyhow::Result<SpotifyPlaylist> {
    let html = st
        .http
        .get(format!("https://open.spotify.com/embed/playlist/{id}"))
        .header("user-agent", UA)
        .header("accept-language", "en")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()
        .context("Spotify didn't find that playlist (is it public?)")?
        .text()
        .await?;
    let json = html
        .split(r#"<script id="__NEXT_DATA__" type="application/json">"#)
        .nth(1)
        .and_then(|s| s.split("</script>").next())
        .context("unexpected Spotify page (no playlist data)")?;
    let v: Value = serde_json::from_str(json).context("reading Spotify's playlist data")?;
    let e = &v["props"]["pageProps"]["state"]["data"]["entity"];
    if e.is_null() {
        bail!("Spotify didn't return that playlist (is it public?)");
    }
    let thumbnail = best_image(&e["coverArt"]["sources"]);
    let tracks: Vec<Track> = e["trackList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let tid = t["uri"].as_str()?.strip_prefix("spotify:track:")?;
            Some(Track {
                video_id: format!("{ID_PREFIX}{tid}"),
                title: t["title"].as_str()?.to_string(),
                artist: t["subtitle"].as_str().unwrap_or_default().replace('\u{a0}', " "),
                album: None,
                duration: t["duration"].as_u64().map(|ms| (ms / 1000) as u32),
                thumbnail: thumbnail.clone(),
            })
        })
        .collect();
    let note = (tracks.len() >= EMBED_LIMIT).then(|| {
        format!(
            "Spotify's public page only lists the first {EMBED_LIMIT} songs. Press Sync now to add the rest through Chosic."
        )
    });
    Ok(SpotifyPlaylist {
        id: id.to_string(),
        name: e["name"].as_str().or(e["title"].as_str()).unwrap_or("Spotify playlist").to_string(),
        owner: e["subtitle"].as_str().filter(|s| !s.is_empty()).map(String::from),
        thumbnail,
        tracks,
        source: "embed".into(),
        synced_at: now(),
        note,
        export_at: None,
    })
}

/// The largest image in a list of `{url, width}` (Spotify lists them in various orders).
fn best_image(sources: &Value) -> Option<String> {
    sources
        .as_array()?
        .iter()
        .filter_map(|s| Some((s["width"].as_u64().unwrap_or(0), s["url"].as_str()?)))
        .max_by_key(|(w, _)| *w)
        .map(|(_, u)| u.to_string())
}

// ---- storage --------------------------------------------------------------------------

async fn stored(st: &AppState, id: &str) -> anyhow::Result<Option<SpotifyPlaylist>> {
    let id = id.to_string();
    let json: Option<String> = st
        .db
        .call(move |c| c.query_row("SELECT data FROM spotify_playlists WHERE id=?1", [id], |r| r.get(0)).optional())
        .await?;
    Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
}

/// Rewrite a stored playlist's list (e.g. with a song's art filled in).
async fn save_json(st: &AppState, p: &SpotifyPlaylist) -> anyhow::Result<()> {
    let (id, json) = (p.id.clone(), serde_json::to_string(p)?);
    st.db.call(move |c| c.execute("UPDATE spotify_playlists SET data=?2 WHERE id=?1", params![id, json]).map(|_| ())).await
}

async fn store(st: &AppState, p: &SpotifyPlaylist) -> anyhow::Result<()> {
    // The songs' details, for playing, history and matching (no YouTube involved).
    st.db.upsert_tracks(p.tracks.clone()).await?;
    let json = serde_json::to_string(p)?;
    let (id, name, owner, thumb, count, at) = (p.id.clone(), p.name.clone(), p.owner.clone(), p.thumbnail.clone(), p.tracks.len() as i64, p.synced_at);
    st.db
        .call(move |c| {
            c.execute(
                "INSERT INTO spotify_playlists(id, name, owner, thumbnail, count, data, synced_at, added_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, owner=excluded.owner, thumbnail=excluded.thumbnail,
                   count=excluded.count, data=excluded.data, synced_at=excluded.synced_at",
                params![id, name, owner, thumb, count, json, at],
            )
            .map(|_| ())
        })
        .await
}

// ---- album art --------------------------------------------------------------------------
//
// The embed page only has the playlist's cover, so its songs show that until they're played:
// the first play looks up the song's own art (one request to Spotify's public oEmbed endpoint)
// and remembers it, for the queue, the stored playlists and later imports.

/// Album art already looked up, by Spotify track id.
async fn known_art(st: &AppState) -> anyhow::Result<std::collections::HashMap<String, String>> {
    st.db
        .call(|c| {
            let mut stmt = c.prepare("SELECT track_id, url FROM spotify_art")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
        .await
}

/// Songs from the embed page get the art already known for them (no requests): looked up when
/// they played, or already in the stored copy (`old`). The rest keep the playlist's cover until
/// they play.
async fn apply_known_art(st: &AppState, p: &mut SpotifyPlaylist, old: Option<&SpotifyPlaylist>) {
    let mut known = known_art(st).await.unwrap_or_default();
    if let Some(o) = old {
        for t in &o.tracks {
            if let (Some(tid), Some(art)) = (track_id(&t.video_id), t.thumbnail.as_deref()) {
                if Some(art) != o.thumbnail.as_deref() {
                    known.entry(tid.to_string()).or_insert_with(|| art.to_string());
                }
            }
        }
    }
    for t in &mut p.tracks {
        if let Some(art) = track_id(&t.video_id).and_then(|tid| known.get(tid)) {
            t.thumbnail = Some(art.clone());
        }
    }
}

/// Song ids whose art is being (or has been) looked up since the server started, so the several
/// requests one play makes (`/source`, ranges of `/stream`) look it up once.
static ART_ASKED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// A Spotify song is playing: look up its album art if that hasn't been done, in the background,
/// and show it everywhere the song is (queue, stored playlists, song details).
pub fn art_on_play(st: crate::state::SharedState, video_id: &str) {
    let Some(tid) = track_id(video_id).map(String::from) else { return };
    if !ART_ASKED.lock().map(|mut s| s.insert(tid.clone())).unwrap_or(false) {
        return;
    }
    let video_id = video_id.to_string();
    tokio::spawn(async move {
        if let Err(e) = fill_art(&st, &video_id, &tid).await {
            tracing::debug!("spotify art for {video_id}: {e:#}");
            // Let a later play try again.
            if let Ok(mut s) = ART_ASKED.lock() {
                s.remove(&tid);
            }
        }
    });
}

async fn fill_art(st: &crate::state::SharedState, video_id: &str, tid: &str) -> anyhow::Result<()> {
    let t = tid.to_string();
    let have: Option<String> = st
        .db
        .call(move |c| c.query_row("SELECT url FROM spotify_art WHERE track_id=?1", [t], |r| r.get(0)).optional())
        .await?;
    let art = match have {
        Some(a) => a,
        None => {
            let a = track_art(st, tid).await.context("no art from Spotify")?;
            let (t, u) = (tid.to_string(), a.clone());
            st.db.call(move |c| c.execute("INSERT OR REPLACE INTO spotify_art(track_id, url) VALUES(?1,?2)", params![t, u]).map(|_| ())).await?;
            a
        }
    };

    // Song details (history, library, next plays).
    if let Some(mut track) = st.db.get_track(video_id).await? {
        if track.thumbnail.as_deref() != Some(art.as_str()) {
            track.thumbnail = Some(art.clone());
            st.db.upsert_tracks(vec![track]).await?;
        }
    }
    // Stored playlists with the song.
    for s in list(st).await? {
        let Some(mut p) = stored(st, &s.id).await? else { continue };
        let mut changed = false;
        for t in p.tracks.iter_mut().filter(|t| t.video_id == video_id) {
            changed |= t.thumbnail.as_deref() != Some(art.as_str());
            t.thumbnail = Some(art.clone());
        }
        if changed {
            save_json(st, &p).await?;
        }
    }
    // The queue: the playing song's cover changes on every device (same item, so no reload).
    let snapshot = {
        let mut q = st.queue.lock().await;
        let mut changed = false;
        for it in q.items.iter_mut().filter(|i| i.track.video_id == video_id) {
            changed |= it.track.thumbnail.as_deref() != Some(art.as_str());
            it.track.thumbnail = Some(art.clone());
        }
        if !changed {
            return Ok(());
        }
        q.version += 1;
        q.save(&st.db).await?;
        q.clone()
    };
    st.emit(crate::state::ServerEvent::Queue { queue: snapshot });
    Ok(())
}

/// A track's album art (300 px) from Spotify's oEmbed endpoint.
async fn track_art(st: &AppState, tid: &str) -> Option<String> {
    let v: Value = st
        .http
        .get("https://open.spotify.com/oembed")
        .query(&[("url", format!("https://open.spotify.com/track/{tid}"))])
        .header("user-agent", UA)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    v["thumbnail_url"].as_str().map(String::from)
}

// ---- playlist exports (Chosic, Exportify, …) ------------------------------------------------
//
// Spotify's public page stops at 100 songs, so a playlist's songs past those come from an export:
// a site such as Chosic's playlist exporter (any playlist, from its link) downloads the whole list
// as CSV, with Spotify track ids and usually album art. (Exportify's files work too.)

/// The Spotify track id in an export's cell: `spotify:track:…`, a track link, or a bare id.
fn cell_track_id(cell: &str) -> Option<&str> {
    let c = cell.trim();
    let id = if let Some(rest) = c.strip_prefix("spotify:track:") {
        rest
    } else if let Some((_, rest)) = c.split_once("open.spotify.com/track/") {
        rest.split(['?', '/', '#']).next().unwrap_or_default()
    } else {
        c
    };
    is_base62_id(id).then_some(id)
}

/// The songs in a Spotify playlist export (CSV from Chosic, Exportify and similar tools). Songs
/// are recognised by their Spotify track id, whichever column holds it.
pub fn parse_exportify(csv_text: &str) -> anyhow::Result<Vec<Track>> {
    let text = csv_text.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers()?.iter().map(|h| h.trim().to_lowercase()).collect();
    let find = |names: &[&str]| headers.iter().position(|h| names.contains(&h.as_str()));
    let id_named = find(&[
        "track uri", "spotify track uri", "uri", "spotify uri", "spotify track id", "spotify id", "track id", "id",
        "spotify track url", "track url", "spotify url", "url", "link",
    ]);
    let title_i = find(&["track name", "song", "song name", "name", "title", "track", "track title"]);
    let artist_i = find(&["artist name(s)", "artist names", "artist name", "artists", "artist", "artist(s)"]);
    let album_i = find(&["album name", "album", "album title"]);
    let dur_ms_i = find(&["track duration (ms)", "duration (ms)", "duration_ms", "duration ms"]);
    let dur_i = find(&["time", "duration", "length", "track duration"]);
    let art_i = find(&[
        "album image url", "spotify track img", "spotify track image", "track image", "album image", "album art url",
        "album art", "image url", "image", "cover", "artwork",
    ]);
    let records: Vec<csv::StringRecord> = rdr.records().collect::<Result<_, _>>()?;
    // No column by a known name: the one whose cells are Spotify track ids.
    let id_i = id_named
        .filter(|&i| records.iter().take(20).any(|r| r.get(i).and_then(cell_track_id).is_some()))
        .or_else(|| (0..headers.len()).find(|&i| records.iter().take(20).filter(|r| r.get(i).and_then(cell_track_id).is_some()).count() >= records.len().min(20).div_ceil(2)))
        .context("This doesn't look like a Spotify playlist export (no Spotify track ids in it)")?;
    let title_i = title_i.context("no song title column")?;

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for rec in &records {
        let get = |i: Option<usize>| i.and_then(|i| rec.get(i)).map(str::trim).filter(|s| !s.is_empty());
        // Local files ("spotify:local:…") have no Spotify track to find.
        let Some(tid) = get(Some(id_i)).and_then(cell_track_id) else { continue };
        let Some(title) = get(Some(title_i)) else { continue };
        if !seen.insert(tid.to_string()) {
            continue;
        }
        let duration = get(dur_ms_i)
            .and_then(|d| d.parse::<f64>().ok())
            .map(|ms| (ms / 1000.0).round() as u32)
            .or_else(|| get(dur_i).and_then(crate::models::parse_duration));
        out.push(Track {
            video_id: format!("{ID_PREFIX}{tid}"),
            title: title.to_string(),
            // Several artists: commas (Exportify, older versions semicolons).
            artist: get(artist_i).unwrap_or_default().replace(';', ", ").replace("\\,", ","),
            album: get(album_i).map(String::from),
            duration,
            thumbnail: get(art_i).filter(|u| u.starts_with("https://")).map(String::from),
        });
    }
    if out.is_empty() {
        bail!("No Spotify songs in that file");
    }
    Ok(out)
}

/// The full list for a stored playlist from an export CSV (Chosic, Exportify…). The first 100 songs keep syncing
/// from Spotify; the rest stay as in the file until a newer export is added.
pub async fn add_export(st: &AppState, id: &str, csv_text: &str) -> anyhow::Result<SpotifyPlaylist> {
    let old = stored(st, id).await?.context("Import the playlist's link first")?;
    let mut tracks = parse_exportify(csv_text)?;
    // Exported from a different playlist? It would share none of the songs Spotify shows.
    if !old.tracks.is_empty() {
        let have: std::collections::HashSet<&str> = old.tracks.iter().map(|t| t.video_id.as_str()).collect();
        if !tracks.iter().any(|t| have.contains(t.video_id.as_str())) {
            bail!("That file is from a different playlist (none of its songs are in “{}”)", old.name);
        }
    }
    let cover = old.thumbnail.clone();
    // Album art from the file: remembered like art looked up on play.
    let arts: Vec<(String, String)> = tracks
        .iter()
        .filter_map(|t| Some((track_id(&t.video_id)?.to_string(), t.thumbnail.clone()?)))
        .collect();
    st.db
        .call(move |c| {
            let tx = c.transaction()?;
            for (tid, url) in arts {
                tx.execute("INSERT OR REPLACE INTO spotify_art(track_id, url) VALUES(?1,?2)", params![tid, url])?;
            }
            tx.commit()
        })
        .await?;
    for t in &mut tracks {
        if t.thumbnail.is_none() {
            t.thumbnail = cover.clone();
        }
    }
    let p = SpotifyPlaylist {
        tracks,
        source: "export".into(),
        synced_at: now(),
        note: None,
        export_at: Some(now()),
        ..old
    };
    store(st, &p).await?;
    Ok(p)
}

// ---- picking up export files by themselves --------------------------------------------
//
// The server usually runs on the computer whose browser downloads the export, so it watches the
// Downloads folder: a new playlist export (CSV) is matched to the imported playlist it shares the most
// songs with and loaded, with nothing to copy or pick. (The export site runs in the browser:
// the "Export" click there is the one step that stays.)

/// Setting: folder(s) to watch instead of the detected Downloads folders (`;`-separated).
pub const EXPORT_DIR_KEY: &str = "exportify.dir";
/// Setting: modification time (unix seconds) of the newest export file already handled.
const EXPORT_SEEN_KEY: &str = "exportify.seen_mtime";

/// The folders watched for playlist exports: the configured ones, else the Downloads folders
/// found on this computer (Linux `~/Downloads`, and Windows' through WSL's `/mnt/c/Users/…`).
pub async fn export_dirs(st: &AppState) -> Vec<std::path::PathBuf> {
    if let Ok(Some(dirs)) = st.db.get_setting(EXPORT_DIR_KEY).await {
        let set: Vec<_> = dirs.split(';').map(str::trim).filter(|d| !d.is_empty()).map(std::path::PathBuf::from).collect();
        if !set.is_empty() {
            return set;
        }
    }
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        out.push(std::path::Path::new(&home).join("Downloads"));
    }
    if let Ok(users) = std::fs::read_dir("/mnt/c/Users") {
        for u in users.flatten() {
            let name = u.file_name().to_string_lossy().to_string();
            if matches!(name.as_str(), "Public" | "Default" | "Default User" | "All Users" | "defaultuser0") {
                continue;
            }
            out.push(u.path().join("Downloads"));
        }
    }
    out.retain(|d| d.is_dir());
    out
}

/// Watch for playlist export downloads (every few seconds; only while Spotify playlists are imported).
pub fn spawn_export_watcher(st: crate::state::SharedState) {
    tokio::spawn(async move {
        // Files from before the server started are left alone (the first time: from 10 minutes
        // before, so an export made just before a restart still counts).
        if st.db.get_setting(EXPORT_SEEN_KEY).await.ok().flatten().is_none() {
            let _ = st.db.set_setting(EXPORT_SEEN_KEY, &(now() - 600).to_string()).await;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if let Err(e) = scan_exports(&st).await {
                tracing::debug!("exportify watch: {e:#}");
            }
        }
    });
}

async fn scan_exports(st: &AppState) -> anyhow::Result<()> {
    let playlists = list(st).await?;
    if playlists.is_empty() {
        return Ok(());
    }
    let seen: i64 = st.db.get_setting(EXPORT_SEEN_KEY).await?.and_then(|s| s.parse().ok()).unwrap_or_else(now);
    let mut newest = seen;
    for dir in export_dirs(st).await {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("csv")) {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            // New since last time, and not still being written.
            if mtime <= seen || now() - mtime < 1 || meta.len() > 64 * 1024 * 1024 {
                continue;
            }
            newest = newest.max(mtime);
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            // Any CSV lands in Downloads; only Spotify playlist exports are of interest.
            if parse_exportify(&text).is_err() {
                continue;
            }
            match load_export_file(st, &playlists, &text).await {
                Ok(Some((name, n))) => tracing::info!("loaded {n} songs for “{name}” from {}", path.display()),
                Ok(None) => tracing::info!("{} doesn't match an imported Spotify playlist", path.display()),
                Err(e) => tracing::warn!("couldn't load {}: {e:#}", path.display()),
            }
        }
    }
    if newest > seen {
        st.db.set_setting(EXPORT_SEEN_KEY, &newest.to_string()).await?;
    }
    Ok(())
}

/// Load an export into the imported playlist it shares the most songs with (at least a few, or
/// most of a short one). Returns that playlist's name and its song count.
async fn load_export_file(st: &AppState, playlists: &[SpotifySummary], text: &str) -> anyhow::Result<Option<(String, usize)>> {
    let songs: std::collections::HashSet<String> = parse_exportify(text)?.into_iter().map(|t| t.video_id).collect();
    let mut best: Option<(usize, &SpotifySummary)> = None;
    for s in playlists {
        let Some(p) = stored(st, &s.id).await? else { continue };
        let shared = p.tracks.iter().filter(|t| songs.contains(&t.video_id)).count();
        let enough = shared >= 5 || (shared > 0 && shared * 2 >= p.tracks.len());
        if enough && best.is_none_or(|(b, _)| shared > b) {
            best = Some((shared, s));
        }
    }
    let Some((_, s)) = best else { return Ok(None) };
    let p = add_export(st, &s.id, text).await?;
    Ok(Some((p.name, p.tracks.len())))
}

/// A resync of a playlist with an export: Spotify's current first 100 songs, then the file's
/// songs past those (the ones Spotify's public page doesn't show).
fn merge_export(fresh: SpotifyPlaylist, old: &SpotifyPlaylist) -> SpotifyPlaylist {
    // Fewer than 100 songs on Spotify's page now: that's the whole playlist.
    if fresh.tracks.len() < EMBED_LIMIT {
        return fresh;
    }
    let head: std::collections::HashSet<&str> = fresh.tracks.iter().map(|t| t.video_id.as_str()).collect();
    // The songs past what Spotify shows are the stored ones after the last song it still shows;
    // stored songs before that which it doesn't show were removed (or moved into the top).
    let last_shown = old.tracks.iter().rposition(|t| head.contains(t.video_id.as_str()));
    let tail: Vec<Track> = old
        .tracks
        .iter()
        .skip(last_shown.map_or(old.tracks.len(), |i| i + 1))
        .filter(|t| !head.contains(t.video_id.as_str()))
        .cloned()
        .collect();
    let when = old.export_at.map(|at| {
        let days = (now() - at) / 86400;
        if days < 1 { "today".to_string() } else if days == 1 { "yesterday".to_string() } else { format!("{days} days ago") }
    });
    let mut tracks = fresh.tracks;
    let extra = tail.len();
    tracks.extend(tail);
    SpotifyPlaylist {
        tracks,
        source: "export".into(),
        note: (extra > 0).then(|| {
            format!(
                "The first {EMBED_LIMIT} songs follow Spotify; the {extra} after them are from your playlist export ({}). Press Sync now to export it again and update those.",
                when.unwrap_or_else(|| "earlier".into())
            )
        }),
        export_at: old.export_at,
        ..fresh
    }
}

/// A playlist fetched again keeps the songs past the first 100 that an export file added.
fn with_export(fresh: SpotifyPlaylist, old: Option<&SpotifyPlaylist>) -> SpotifyPlaylist {
    match old {
        Some(o) if o.source == "export" => merge_export(fresh, o),
        _ => fresh,
    }
}

/// Import (or re-import) a playlist from a link.
pub async fn import(st: &AppState, link: &str) -> anyhow::Result<SpotifyPlaylist> {
    let id = playlist_id(link).context("That isn't a Spotify playlist link")?;
    let old = stored(st, &id).await?;
    let mut p = with_export(fetch(st, &id).await?, old.as_ref());
    apply_known_art(st, &mut p, old.as_ref()).await;
    store(st, &p).await?;
    Ok(p)
}

/// A stored playlist, fetched again from Spotify first when it was last synced a while ago (or
/// `refresh`). If Spotify can't be reached the stored list is returned, with a note.
pub async fn open(st: &AppState, id: &str, refresh: bool) -> anyhow::Result<Option<SpotifyPlaylist>> {
    let Some(old) = stored(st, id).await? else { return Ok(None) };
    if !refresh && now() - old.synced_at < RESYNC_AFTER {
        return Ok(Some(old));
    }
    match fetch(st, id).await {
        Ok(p) => {
            let mut p = with_export(p, Some(&old));
            apply_known_art(st, &mut p, Some(&old)).await;
            store(st, &p).await?;
            Ok(Some(p))
        }
        Err(e) => {
            tracing::warn!("spotify resync of {id} failed: {e:#}");
            Ok(Some(SpotifyPlaylist { note: Some(format!("Couldn't resync with Spotify ({e:#}); showing the list from the last sync.")), ..old }))
        }
    }
}

pub async fn list(st: &AppState) -> anyhow::Result<Vec<SpotifySummary>> {
    st.db
        .call(|c| {
            let mut stmt =
                c.prepare("SELECT id, name, owner, thumbnail, count, synced_at FROM spotify_playlists ORDER BY added_at DESC")?;
            let rows = stmt.query_map([], |r| {
                Ok(SpotifySummary {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    owner: r.get(2)?,
                    thumbnail: r.get(3)?,
                    count: r.get::<_, i64>(4)? as usize,
                    synced_at: r.get(5)?,
                })
            })?;
            rows.collect()
        })
        .await
}

pub async fn remove(st: &AppState, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    st.db.call(move |c| c.execute("DELETE FROM spotify_playlists WHERE id=?1", [id]).map(|_| ())).await
}

// ---- playing --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_links() {
        let id = "2hEhPObtDi7z32POi9jr0S";
        assert_eq!(playlist_id("https://open.spotify.com/playlist/2hEhPObtDi7z32POi9jr0S?si=qP5QMJnKR-2FmTwhwr9LJw").as_deref(), Some(id));
        assert_eq!(playlist_id("https://open.spotify.com/intl-de/playlist/2hEhPObtDi7z32POi9jr0S").as_deref(), Some(id));
        assert_eq!(playlist_id("https://open.spotify.com/embed/playlist/2hEhPObtDi7z32POi9jr0S").as_deref(), Some(id));
        assert_eq!(playlist_id("spotify:playlist:2hEhPObtDi7z32POi9jr0S").as_deref(), Some(id));
        assert_eq!(playlist_id(id).as_deref(), Some(id));
        assert_eq!(playlist_id("https://open.spotify.com/album/2hEhPObtDi7z32POi9jr0S"), None);
        assert_eq!(playlist_id("hello"), None);
    }

    #[test]
    fn exportify_csv() {
        let csv = "\u{feff}\"Track URI\",\"Track Name\",\"Artist URI(s)\",\"Artist Name(s)\",\"Album Name\",\"Album Image URL\",\"Track Duration (ms)\"\n\
            \"spotify:track:3OYqNt713cxVT3R0idEfZp\",\"V Limo (New Donk Shitty)\",\"spotify:artist:x\",\"Bill Kiley\",\"Cyberpunk 2077\",\"https://i.scdn.co/image/abc\",\"102933\"\n\
            \"spotify:local:Someone:Local:Song:200\",\"Local Song\",\"\",\"Someone\",\"\",\"\",\"200000\"\n\
            \"spotify:track:6NfXJzeyMsV48pM6zegv0s\",\"I Am A Man Of Constant Sorrow - With Band\",\"spotify:artist:a,spotify:artist:b\",\"The Soggy Bottom Boys,Dan Tyminski\",\"O Brother\",\"\",\"256000\"\n\
            \"spotify:track:3OYqNt713cxVT3R0idEfZp\",\"V Limo (New Donk Shitty)\",\"\",\"Bill Kiley\",\"\",\"\",\"102933\"\n";
        let t = parse_exportify(csv).unwrap();
        assert_eq!(t.len(), 2, "local files and repeats are skipped");
        assert_eq!(t[0].video_id, "spotify_3OYqNt713cxVT3R0idEfZp");
        assert_eq!(t[0].duration, Some(103));
        assert_eq!(t[0].thumbnail.as_deref(), Some("https://i.scdn.co/image/abc"));
        assert_eq!(t[1].artist, "The Soggy Bottom Boys,Dan Tyminski");
        assert!(t[1].thumbnail.is_none());
        assert!(parse_exportify("Title,Artist\nA,B\n").is_err());
    }

    #[test]
    fn chosic_style_csv() {
        // Bare track ids, "Song", "Time" as m:ss, "Spotify Track Img".
        let csv = "#,Song,Artist,Popularity,Album,Time,Added At,Spotify Track Id,Spotify Track Img\n\
            1,V Limo (New Donk Shitty),Bill Kiley,40,Cyberpunk 2077,1:43,2024-01-01,3OYqNt713cxVT3R0idEfZp,https://i.scdn.co/image/abc\n\
            2,Chinatown,Bill Kiley,38,Cyberpunk 2077,6:02,2024-01-01,4pfwYNLVhn9NVHQ9yew18M,\n";
        let t = parse_exportify(csv).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].video_id, "spotify_3OYqNt713cxVT3R0idEfZp");
        assert_eq!(t[0].duration, Some(103));
        assert_eq!(t[0].thumbnail.as_deref(), Some("https://i.scdn.co/image/abc"));
        assert_eq!(t[1].duration, Some(362));
        // Track links in a column with an unknown name.
        let csv = "Name,Performer,Where\nChinatown,Bill Kiley,https://open.spotify.com/track/4pfwYNLVhn9NVHQ9yew18M?si=x\n";
        assert_eq!(parse_exportify(csv).unwrap()[0].video_id, "spotify_4pfwYNLVhn9NVHQ9yew18M");
        // Not a Spotify export.
        assert!(parse_exportify("date,amount\n2026-01-01,5\n").is_err());
    }

    fn song(n: usize) -> Track {
        Track { video_id: format!("{ID_PREFIX}{n:022}"), title: format!("song {n}"), artist: "x".into(), album: None, duration: None, thumbnail: None }
    }

    fn playlist(tracks: Vec<Track>, source: &str) -> SpotifyPlaylist {
        SpotifyPlaylist {
            id: "p".into(),
            name: "P".into(),
            owner: None,
            thumbnail: None,
            tracks,
            source: source.into(),
            synced_at: 0,
            note: None,
            export_at: Some(now()),
        }
    }

    #[test]
    fn export_merge() {
        // Stored: 150 songs (100 from Spotify + 50 from the file). On Spotify now, song 3 was
        // removed, so song 100 (the 101st) moved into the first 100.
        let old = playlist((0..150).map(song).collect(), "export");
        let fresh = playlist((0..101).filter(|n| *n != 3).map(song).collect(), "embed");
        let m = merge_export(fresh, &old);
        assert_eq!(m.source, "export");
        assert_eq!(m.tracks.len(), 149);
        let ids: std::collections::HashSet<_> = m.tracks.iter().map(|t| &t.video_id).collect();
        assert_eq!(ids.len(), 149, "no duplicates");
        assert!(!ids.contains(&song(3).video_id));
        assert_eq!(m.tracks.last().unwrap().video_id, song(149).video_id);
        assert!(m.note.is_some());
        // The file's first 100 don't line up with Spotify's (the file lacks song 3, which Spotify
        // shows): the file's songs after the last one Spotify shows are all kept.
        let old = playlist((0..130).filter(|n| *n != 3).map(song).collect(), "export");
        let fresh = playlist((0..100).map(song).collect(), "embed");
        let m = merge_export(fresh, &old);
        assert_eq!(m.tracks.len(), 130);
        assert_eq!(m.tracks[100].video_id, song(100).video_id);
        // Spotify shows fewer than 100 now: that's the whole playlist.
        let m = merge_export(playlist((0..50).map(song).collect(), "embed"), &old);
        assert_eq!((m.tracks.len(), m.source.as_str()), (50, "embed"));
    }

    #[test]
    fn song_ids() {
        assert!(is_spotify_id("spotify_3OYqNt713cxVT3R0idEfZp"));
        assert!(!is_spotify_id("spotify_short"));
        assert!(!is_spotify_id("dQw4w9WgXcQ"));
        assert_eq!(track_id("spotify_3OYqNt713cxVT3R0idEfZp"), Some("3OYqNt713cxVT3R0idEfZp"));
    }
}
