//! Where audio comes from, in the order you choose: Plex, Jellyfin, the music folder and YouTube
//! (which includes the MP3 cache of YouTube plays). Any of them can be turned off; with YouTube
//! off the app runs on the library sources only.
//!
//! The music folder is on the device running the web app: the app scans and plays it itself, so
//! the server only keeps its place in the list (and never plays from it). Songs only in a
//! device's folder have `local_<16 hex>` ids, which the server treats like imported songs.
//!
//! The library sources share a few helpers here (finding a song's copy, searching them all,
//! matching a song by its tags), so adding another source means one more arm in each `match`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{
    db::Db,
    models::Track,
    services::{
        jellyfin::{self, JellyfinTrack},
        matching,
        plex::{self, PlexTrack},
    },
    state::AppState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Plex,
    Jellyfin,
    Local,
    Youtube,
}

/// Every source, in the default priority order.
pub const ALL: [Source; 4] = [Source::Plex, Source::Jellyfin, Source::Local, Source::Youtube];

impl Source {
    /// The name used in the API (`x-audio-source`, `/source`, prefs).
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Plex => "plex",
            Source::Jellyfin => "jellyfin",
            Source::Local => "local",
            Source::Youtube => "youtube",
        }
    }

    /// How it's named in messages.
    pub fn label(self) -> &'static str {
        match self {
            Source::Plex => "Plex",
            Source::Jellyfin => "Jellyfin",
            Source::Local => "the music folder",
            Source::Youtube => "YouTube",
        }
    }

    /// A library of files (everything but YouTube).
    pub fn is_library(self) -> bool {
        self != Source::Youtube
    }
}

/// The error YouTube-only endpoints answer with while YouTube is off.
pub const YOUTUBE_OFF: &str = "YouTube is turned off in Settings → Audio sources";

/// The server-side library a song that's only there belongs to (`plex_…`, `jf_…` ids).
pub fn own_source(video_id: &str) -> Option<Source> {
    if plex::is_plex_id(video_id) {
        Some(Source::Plex)
    } else if jellyfin::is_jellyfin_id(video_id) {
        Some(Source::Jellyfin)
    } else {
        None
    }
}

/// A song that's only in a library (never looked up on YouTube).
pub fn is_library_id(video_id: &str) -> bool {
    own_source(video_id).is_some()
}

// ---- the priority list ----------------------------------------------------------------------

/// One entry of the priority list (as stored and sent to the app).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePref {
    pub id: Source,
    pub on: bool,
}

/// Settings key: the priority list as JSON.
pub const PREF_KEY: &str = "prefs.sources";
/// The old "Prefer Plex" switch, read once to seed the list.
const OLD_PREFER_PLEX: &str = "prefs.prefer_plex";

/// The list as given, with each source once (first one wins) and any missing ones added at the
/// end, turned on.
fn complete(list: Vec<SourcePref>) -> Vec<SourcePref> {
    let mut seen = HashSet::new();
    let mut out: Vec<SourcePref> = list.into_iter().filter(|p| seen.insert(p.id)).collect();
    out.extend(ALL.into_iter().filter(|s| !seen.contains(s)).map(|id| SourcePref { id, on: true }));
    out
}

/// The list from what's stored. Before there was one, "Prefer Plex" off meant YouTube first.
fn parse(stored: Option<&str>, prefer_plex: Option<&str>) -> Vec<SourcePref> {
    if let Some(list) = stored.and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok()) {
        // Entries this version doesn't know (a source from a newer one) are skipped.
        return complete(list.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect());
    }
    let youtube_first = matches!(prefer_plex, Some("0" | "false"));
    let order = if youtube_first {
        vec![Source::Youtube, Source::Plex, Source::Jellyfin, Source::Local]
    } else {
        ALL.to_vec()
    };
    order.into_iter().map(|id| SourcePref { id, on: true }).collect()
}

/// A list sent by the app must name every source exactly once.
pub fn validate(list: &[SourcePref]) -> Result<(), String> {
    let mut seen = HashSet::new();
    if let Some(dup) = list.iter().find(|p| !seen.insert(p.id)) {
        return Err(format!("sources lists {} twice", dup.id.as_str()));
    }
    if let Some(missing) = ALL.iter().find(|s| !seen.contains(s)) {
        return Err(format!("sources must list every source ({} is missing)", missing.as_str()));
    }
    Ok(())
}

/// The priority list, all four sources, first = tried first.
pub async fn prefs(db: &Db) -> Vec<SourcePref> {
    let stored = db.get_setting(PREF_KEY).await.ok().flatten();
    let old = db.get_setting(OLD_PREFER_PLEX).await.ok().flatten();
    parse(stored.as_deref(), old.as_deref())
}

pub async fn set_prefs(db: &Db, list: Vec<SourcePref>) -> anyhow::Result<Vec<SourcePref>> {
    validate(&list).map_err(anyhow::Error::msg)?;
    db.set_setting(PREF_KEY, &serde_json::to_string(&list)?).await?;
    Ok(list)
}

/// The sources that are on, in order.
pub async fn order(db: &Db) -> Vec<Source> {
    prefs(db).await.into_iter().filter(|p| p.on).map(|p| p.id).collect()
}

pub async fn youtube_enabled(db: &Db) -> bool {
    order(db).await.contains(&Source::Youtube)
}

// ---- which source plays a song ----------------------------------------------------------------

/// The sources to try for a song, in order. `own`: the library a source-only song belongs to,
/// tried first even when it's off (it's the only place the song is sure to be), then the other
/// libraries that are on; such a song never goes to YouTube. Other songs: the sources that are
/// on, skipping libraries that aren't set up (`ready`).
pub fn plan(own: Option<Source>, order: &[Source], ready: &[Source]) -> Vec<Source> {
    let mut out: Vec<Source> = own.into_iter().collect();
    for &s in order {
        let usable = s == Source::Youtube || ready.contains(&s);
        if usable && !out.contains(&s) && !(own.is_some() && s == Source::Youtube) {
            out.push(s);
        }
    }
    out
}

/// "A", "A or B", "A, B or C".
fn join_or(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// Why nothing plays a song when every source tried said it doesn't have it.
pub fn not_found_message(tried: &[Source]) -> String {
    if tried.is_empty() {
        return "No audio source is on. Turn one on in Settings → Audio sources.".into();
    }
    let labels: Vec<&str> = tried.iter().map(|s| s.label()).collect();
    format!("Not found in {}", join_or(&labels))
}

/// Library sources the server can play that are connected (whether or not they're on). Never
/// the music folder: that's played by the web app.
pub async fn ready(st: &AppState) -> Vec<Source> {
    let mut out = Vec::new();
    if st.plex.server().await.is_some() {
        out.push(Source::Plex);
    }
    if st.jellyfin.configured().await {
        out.push(Source::Jellyfin);
    }
    out
}

/// The sources to try for `video_id`, in order (see [`plan`]).
pub async fn play_plan(st: &AppState, video_id: &str) -> Vec<Source> {
    plan(own_source(video_id), &order(&st.db).await, &ready(st).await)
}

/// Library sources that are on and set up, in priority order.
pub async fn libraries(st: &AppState) -> Vec<Source> {
    plan(None, &order(&st.db).await, &ready(st).await).into_iter().filter(|s| s.is_library()).collect()
}

// ---- a song's copy in a library -----------------------------------------------------------------

/// The file a song plays from in one of the libraries.
#[derive(Debug, Clone)]
pub enum LibraryItem {
    Plex(PlexTrack),
    Jellyfin(JellyfinTrack),
}

/// Which library file a song plays from (`item` in `/api/tracks/{id}/source`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemInfo {
    pub source: Source,
    /// Plex rating key or Jellyfin item id.
    pub item_id: String,
    pub title: String,
    pub artist: String,
}

impl LibraryItem {
    pub fn source(&self) -> Source {
        match self {
            LibraryItem::Plex(_) => Source::Plex,
            LibraryItem::Jellyfin(_) => Source::Jellyfin,
        }
    }

    pub fn info(&self) -> ItemInfo {
        let (item_id, title, artist) = match self {
            LibraryItem::Plex(t) => (t.rating_key.clone(), &t.title, &t.artist),
            LibraryItem::Jellyfin(t) => (t.item_id.clone(), &t.title, &t.artist),
        };
        ItemInfo { source: self.source(), item_id, title: title.clone(), artist: artist.clone() }
    }
}

/// A song's copy in library `s`: the song itself when it's only there, else the copy matched by
/// its tags (`track`; None when its tags aren't known). Ok(None): not in that library.
pub async fn find_item(st: &AppState, s: Source, video_id: &str, track: Option<&Track>) -> anyhow::Result<Option<LibraryItem>> {
    if own_source(video_id) == Some(s) {
        return Ok(match s {
            Source::Plex => st.plex.track_for_id(video_id).await?.map(LibraryItem::Plex),
            Source::Jellyfin => st.jellyfin.track_for_id(video_id).await?.map(LibraryItem::Jellyfin),
            Source::Local | Source::Youtube => None,
        });
    }
    let Some(track) = track else { return Ok(None) };
    Ok(match s {
        Source::Plex => st.plex.find_match(track).await?.map(LibraryItem::Plex),
        Source::Jellyfin => st.jellyfin.find_match(track).await?.map(LibraryItem::Jellyfin),
        Source::Local | Source::Youtube => None,
    })
}

/// A possible library copy of a song (for picking one by hand), same shape as Plex's.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The library track, under its source-only id (`jf_…`).
    pub track: Track,
    pub score: f32,
    pub live: bool,
}

/// How a song's copy in a library source is chosen (`GET /api/jellyfin/matches/{id}`),
/// same JSON shape as Plex's `PlexChoice`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub mode: plex::MatchMode,
    /// Item id it plays from now (in automatic mode, also one not yet stored).
    pub current: Option<String>,
    /// Possible copies, best first.
    pub candidates: Vec<Candidate>,
}

// ---- searching and matching across the libraries ------------------------------------------------

/// Songs in library `s` matching a search, each with its album artist (for filtering).
async fn search_source(st: &AppState, s: Source, query: &str) -> anyhow::Result<Vec<(Track, String)>> {
    Ok(match s {
        Source::Plex => st.plex.search(query).await?.iter().map(|h| (h.to_track(), h.album_artist.clone())).collect(),
        Source::Jellyfin => st.jellyfin.search(query).await?.iter().map(|h| (h.to_track(), h.album_artist.clone())).collect(),
        Source::Local | Source::Youtube => Vec::new(),
    })
}

/// Same song, whatever its id (for keeping one copy of each).
pub fn name_key(t: &Track) -> String {
    format!("{}|{}", matching::primary_artist(&t.artist), matching::normalize(&t.title))
}

/// Songs from every library that's on, in priority order (a song in two libraries comes from the
/// first), under their source-only ids. Library searches are loose (parts of words, other
/// fields), so only songs containing every word typed are kept.
pub async fn search_library(st: &AppState, query: &str, limit: usize) -> Vec<Track> {
    let words: Vec<String> = matching::normalize(query).split_whitespace().map(String::from).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let libs = libraries(st).await;
    let results = futures::future::join_all(libs.iter().map(|&s| search_source(st, s, query))).await;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (s, res) in libs.iter().zip(results) {
        let hits = match res {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("{} search failed: {e:#}", s.as_str());
                continue;
            }
        };
        for (t, album_artist) in hits {
            let hay = matching::normalize(&format!(
                "{} {album_artist} {} {}",
                t.artist,
                t.title,
                t.album.as_deref().unwrap_or_default()
            ));
            if words.iter().all(|w| hay.contains(w.as_str())) && seen.insert(name_key(&t)) {
                out.push(t);
            }
        }
    }
    out.truncate(limit);
    if let Err(e) = st.db.upsert_tracks(out.clone()).await {
        tracing::warn!("saving library results: {e:#}");
    }
    out
}

/// The library copy of a song known only by its tags (an import, a Last.fm suggestion): the best
/// match in the first library that has one, as a song under its source-only id (saved).
pub async fn best_match(st: &AppState, want: &Track) -> Option<Track> {
    fn best<H>(found: Vec<(H, f32)>, threshold: f32, to_track: impl Fn(&H) -> Track) -> Option<Track> {
        found.iter().find(|(_, s)| *s >= threshold).map(|(h, _)| to_track(h))
    }
    for s in libraries(st).await {
        let found = match s {
            Source::Plex => st.plex.candidates(want).await.map(|c| best(c, plex::MATCH_THRESHOLD, |h| h.to_track())),
            Source::Jellyfin => {
                st.jellyfin.candidates(want).await.map(|c| best(c, jellyfin::MATCH_THRESHOLD, |h| h.to_track()))
            }
            Source::Local | Source::Youtube => Ok(None),
        };
        match found {
            Ok(Some(t)) => {
                let _ = st.db.upsert_tracks(vec![t.clone()]).await;
                return Some(t);
            }
            Ok(None) => {}
            Err(e) => tracing::debug!("{} match for {} - {} failed: {e:#}", s.as_str(), want.artist, want.title),
        }
    }
    None
}

/// Random songs from library `s`: for Plex and Jellyfin, songs of theirs the app already knows
/// (searched, imported, played), which costs no request.
pub async fn random_tracks(st: &AppState, s: Source, n: usize) -> Vec<Track> {
    let n = n as i64;
    let rows = match s {
        Source::Plex | Source::Jellyfin => {
            let prefix = if s == Source::Plex { plex::ID_PREFIX } else { jellyfin::ID_PREFIX };
            st.db
                .call(move |c| {
                    let mut stmt = c.prepare(
                        "SELECT video_id,title,artist,album,duration,thumbnail FROM tracks
                         WHERE substr(video_id, 1, length(?1)) = ?1 ORDER BY RANDOM() LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(rusqlite::params![prefix, n], crate::db::row_to_track)?;
                    rows.collect()
                })
                .await
        }
        Source::Local | Source::Youtube => Ok(Vec::new()),
    };
    rows.unwrap_or_else(|e| {
        tracing::warn!("random {} songs: {e:#}", s.as_str());
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use Source::*;

    fn ids(list: &[SourcePref]) -> Vec<(Source, bool)> {
        list.iter().map(|p| (p.id, p.on)).collect()
    }

    #[test]
    fn defaults_and_migration() {
        assert_eq!(ids(&parse(None, None)), [(Plex, true), (Jellyfin, true), (Local, true), (Youtube, true)]);
        assert_eq!(ids(&parse(None, Some("1"))), ids(&parse(None, None)));
        // "Prefer Plex" off: YouTube first.
        assert_eq!(ids(&parse(None, Some("0"))), [(Youtube, true), (Plex, true), (Jellyfin, true), (Local, true)]);
        // A stored list wins over the old switch.
        let stored = r#"[{"id":"local","on":true},{"id":"youtube","on":false},{"id":"plex","on":true},{"id":"jellyfin","on":false}]"#;
        assert_eq!(ids(&parse(Some(stored), Some("0"))), [(Local, true), (Youtube, false), (Plex, true), (Jellyfin, false)]);
    }

    #[test]
    fn stored_lists_are_completed() {
        // Missing sources are added (on), duplicates and unknown ones dropped.
        let stored = r#"[{"id":"youtube","on":false},{"id":"tidal","on":true},{"id":"youtube","on":true},{"id":"local","on":true}]"#;
        assert_eq!(ids(&parse(Some(stored), None)), [(Youtube, false), (Local, true), (Plex, true), (Jellyfin, true)]);
        // Garbage: the default.
        assert_eq!(ids(&parse(Some("not json"), None)), ids(&parse(None, None)));
    }

    #[test]
    fn validation() {
        let p = |id, on| SourcePref { id, on };
        assert!(validate(&[p(Youtube, false), p(Local, true), p(Plex, true), p(Jellyfin, true)]).is_ok());
        assert!(validate(&[p(Youtube, true), p(Local, true), p(Plex, true)]).unwrap_err().contains("jellyfin is missing"));
        assert!(validate(&[p(Youtube, true), p(Youtube, false), p(Local, true), p(Plex, true), p(Jellyfin, true)])
            .unwrap_err()
            .contains("twice"));
    }

    #[test]
    fn play_order() {
        let all_ready = [Plex, Jellyfin, Local];
        // Any song: the sources that are on, in order.
        assert_eq!(plan(None, &[Plex, Jellyfin, Local, Youtube], &all_ready), [Plex, Jellyfin, Local, Youtube]);
        assert_eq!(plan(None, &[Youtube, Local], &all_ready), [Youtube, Local]);
        // Libraries that aren't set up are skipped; YouTube needs no setup.
        assert_eq!(plan(None, &[Plex, Jellyfin, Local, Youtube], &[Local]), [Local, Youtube]);
        // YouTube off and nothing set up: nothing to try.
        assert!(plan(None, &[Plex, Jellyfin], &[]).is_empty());
        // A Jellyfin-only song: Jellyfin first (even when it's off), then the other libraries, never YouTube.
        assert_eq!(plan(Some(Jellyfin), &[Youtube, Plex, Local], &all_ready), [Jellyfin, Plex, Local]);
        assert_eq!(plan(Some(Local), &[Local, Youtube], &[]), [Local]);
    }

    #[test]
    fn source_only_ids() {
        assert_eq!(own_source("plex_123"), Some(Plex));
        assert_eq!(own_source("jf_0123456789abcdef0123456789abcdef"), Some(Jellyfin));
        // A device-folder song: the server plays it like an imported song.
        assert_eq!(own_source("local_0123456789abcdef"), None);
        assert!(crate::services::resolver::is_deferred_id("local_0123456789abcdef"));
        assert_eq!(own_source("dQw4w9WgXcQ"), None);
        assert_eq!(own_source("spotify_3OYqNt713cxVT3R0idEfZp"), None);
    }

    #[test]
    fn messages() {
        assert_eq!(not_found_message(&[Plex]), "Not found in Plex");
        assert_eq!(not_found_message(&[Plex, Youtube]), "Not found in Plex or YouTube");
        assert_eq!(not_found_message(&[Plex, Jellyfin, Youtube]), "Not found in Plex, Jellyfin or YouTube");
        assert!(not_found_message(&[]).contains("No audio source is on"));
    }
}
