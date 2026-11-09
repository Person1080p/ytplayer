//! Recommendations: YouTube Music radio blended with Last.fm similar tracks. With YouTube off,
//! the same from the library sources (Plex, Jellyfin, the local folder).

use std::collections::HashSet;

use futures::{stream, StreamExt};
use rand::seq::SliceRandom;

use crate::{
    models::{HomeItem, Shelf, Track},
    services::{
        matching, resolver,
        sources::{self, name_key, Source},
    },
    state::AppState,
};

/// The YouTube video to seed radio with. A library-only song has none of its own, so the closest
/// YouTube Music search result stands in for it (None when there isn't a convincing one); a
/// Spotify or imported song uses the video it plays from. None with YouTube off.
pub async fn youtube_seed(state: &AppState, seed: &Track) -> Option<String> {
    if !sources::youtube_enabled(&state.db).await {
        return None;
    }
    if resolver::is_deferred_id(&seed.video_id) {
        return resolver::deferred_youtube_id(state, &seed.video_id).await.ok().flatten();
    }
    if !sources::is_library_id(&seed.video_id) {
        return Some(seed.video_id.clone());
    }
    let query = format!("{} {}", matching::primary_artist(&seed.artist), seed.title);
    let results = state.yt.search_songs(&query, 5).await.ok()?;
    results
        .into_iter()
        .map(|t| (matching::score(&seed.title, &seed.artist, seed.duration, &t.title, &t.artist, t.duration), t))
        .filter(|(s, _)| *s >= 0.6)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, t)| t.video_id)
}

/// Last.fm suggestions resolved to YouTube tracks.
async fn resolve_many(state: &AppState, pairs: Vec<(String, String)>) -> Vec<Track> {
    stream::iter(pairs)
        .map(|(artist, title)| async move {
            resolver::resolve(&state.yt, &state.db, &artist, &title, None).await.ok().flatten()
        })
        .buffered(4)
        .filter_map(|t| async move { t })
        .collect()
        .await
}

pub async fn lastfm_similar(state: &AppState, seed: &Track, n: usize) -> Vec<Track> {
    if state.lastfm.keys().await.is_none() || seed.artist.is_empty() {
        return vec![];
    }
    match state.lastfm.similar_tracks(&seed.artist, &seed.title, (n * 2) as u32).await {
        Ok(sim) => resolve_many(state, sim.into_iter().take(n).map(|s| (s.artist, s.title)).collect()).await,
        Err(e) => {
            tracing::debug!("last.fm similar failed: {e:#}");
            vec![]
        }
    }
}

/// Up to `limit` tracks to follow `seed`, excluding `exclude` video ids.
pub async fn for_track(state: &AppState, seed: &Track, limit: usize, exclude: &HashSet<String>) -> anyhow::Result<Vec<Track>> {
    if !sources::youtube_enabled(&state.db).await {
        return library_for_track(state, seed, limit, exclude).await;
    }
    let radio = async {
        match youtube_seed(state, seed).await {
            Some(id) => state.yt.watch_playlist(Some(&id), None, limit + 10).await,
            None => Ok(vec![]),
        }
    };
    let (yt, lf) = tokio::join!(radio, lastfm_similar(state, seed, limit / 3 + 2));
    let yt = yt.unwrap_or_else(|e| {
        tracing::warn!("youtube radio failed: {e:#}");
        vec![]
    });

    let mut seen_ids: HashSet<String> = exclude.clone();
    seen_ids.insert(seed.video_id.clone());
    let mut seen_names: HashSet<String> = HashSet::new();
    seen_names.insert(name_key(seed));

    // Interleave 2 radio : 1 last.fm, so the mix stays coherent but still branches out.
    let mut out = Vec::with_capacity(limit);
    let (mut yi, mut li) = (yt.into_iter(), lf.into_iter());
    let mut turn = 0usize;
    while out.len() < limit {
        let next = if turn % 3 == 2 { li.next().or_else(|| yi.next()) } else { yi.next().or_else(|| li.next()) };
        let Some(t) = next else { break };
        turn += 1;
        if seen_ids.insert(t.video_id.clone()) && seen_names.insert(name_key(&t)) {
            out.push(t);
        }
    }
    state.db.upsert_tracks(out.clone()).await?;
    Ok(out)
}

// ---- from the library sources (YouTube off) ---------------------------------------------------

/// Up to `limit` library songs to follow `seed` (excluding `exclude` ids): Last.fm's similar
/// tracks that are in the libraries, then the same artist's songs, then random library songs.
/// Never asks YouTube.
pub async fn library_for_track(
    state: &AppState,
    seed: &Track,
    limit: usize,
    exclude: &HashSet<String>,
) -> anyhow::Result<Vec<Track>> {
    let (similar, mut by_artist) = tokio::join!(library_similar(state, seed, limit), library_by_artist(state, seed));
    by_artist.shuffle(&mut rand::thread_rng());

    let mut seen_ids: HashSet<String> = exclude.clone();
    seen_ids.insert(seed.video_id.clone());
    let mut seen_names: HashSet<String> = HashSet::from([name_key(seed)]);
    let mut out = Vec::with_capacity(limit);
    let mut add = |out: &mut Vec<Track>, t: Track| {
        if out.len() < limit && seen_ids.insert(t.video_id.clone()) && seen_names.insert(name_key(&t)) {
            out.push(t);
        }
    };
    for t in similar {
        add(&mut out, t);
    }
    // At most half the same artist, so it doesn't turn into one artist's album.
    let artist_cap = out.len() + limit.div_ceil(2);
    for t in by_artist {
        if out.len() >= artist_cap {
            break;
        }
        add(&mut out, t);
    }
    if out.len() < limit {
        for t in random_library(state, (limit - out.len()) * 2).await {
            add(&mut out, t);
        }
    }
    state.db.upsert_tracks(out.clone()).await?;
    Ok(out)
}

/// Last.fm's tracks similar to `seed` that are in the libraries.
async fn library_similar(state: &AppState, seed: &Track, n: usize) -> Vec<Track> {
    if state.lastfm.keys().await.is_none() || seed.artist.is_empty() {
        return vec![];
    }
    let similar = match state.lastfm.similar_tracks(&seed.artist, &seed.title, (n * 2) as u32).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("last.fm similar failed: {e:#}");
            return vec![];
        }
    };
    stream::iter(similar.into_iter().take(n))
        .map(|s| async move {
            let want = Track { video_id: String::new(), title: s.title, artist: s.artist, album: None, duration: None, thumbnail: None };
            sources::best_match(state, &want).await
        })
        .buffered(4)
        .filter_map(|t| async move { t })
        .collect()
        .await
}

/// The libraries' songs by `seed`'s artist.
async fn library_by_artist(state: &AppState, seed: &Track) -> Vec<Track> {
    let artist = matching::primary_artist(&seed.artist);
    if artist.is_empty() {
        return vec![];
    }
    let mut found = sources::search_library(state, &artist, 100).await;
    found.retain(|t| matching::primary_artist(&t.artist) == artist);
    found
}

/// Random songs from the libraries that are on.
async fn random_library(state: &AppState, n: usize) -> Vec<Track> {
    let mut out = Vec::new();
    for s in sources::libraries(state).await {
        out.extend(sources::random_tracks(state, s, n).await);
    }
    out.shuffle(&mut rand::thread_rng());
    out
}

/// The home screen with YouTube off: what you played, your library, more by the artist you
/// played last, and random songs from each library.
pub async fn library_home(state: &AppState) -> anyhow::Result<Vec<Shelf>> {
    let profile = state.db.active_profile().await;
    let recent: Vec<Track> = state
        .db
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT t.video_id, t.title, t.artist, t.album, t.duration, t.thumbnail
                 FROM history h JOIN tracks t ON t.video_id = h.video_id
                 WHERE h.profile = ?1
                 GROUP BY h.video_id ORDER BY MAX(h.played_at) DESC LIMIT 20",
            )?;
            let rows = stmt.query_map([profile], crate::db::row_to_track)?;
            rows.collect()
        })
        .await?;
    let liked: Vec<Track> = state
        .db
        .call(|c| {
            let mut stmt = c.prepare(
                "SELECT t.video_id, t.title, t.artist, t.album, t.duration, t.thumbnail
                 FROM liked l JOIN tracks t ON t.video_id = l.video_id
                 GROUP BY t.video_id ORDER BY MAX(l.added_at) DESC LIMIT 20",
            )?;
            let rows = stmt.query_map([], crate::db::row_to_track)?;
            rows.collect()
        })
        .await?;

    let mut shelves = Vec::new();
    let mut shelf = |title: String, tracks: Vec<Track>| {
        if !tracks.is_empty() {
            shelves.push(Shelf { title, items: tracks.into_iter().map(song_item).collect() });
        }
    };
    let last_artist = recent.first().map(|t| t.artist.clone());
    shelf("Recently played".into(), recent);
    shelf("Your library".into(), liked);
    if let Some(artist) = last_artist.filter(|a| !a.is_empty()) {
        let mut more = library_by_artist(state, &Track { artist: artist.clone(), ..empty_track() }).await;
        more.truncate(20);
        shelf(format!("More by {}", first_artist(&artist)), more);
    }
    for s in sources::libraries(state).await {
        let title = match s {
            Source::Local => "From your music folder".to_string(),
            other => format!("From {}", other.label()),
        };
        shelf(title, sources::random_tracks(state, s, 20).await);
    }
    Ok(shelves)
}

/// The first listed artist as written ("A, B & C" -> "A").
fn first_artist(s: &str) -> &str {
    s.split([',', ';', '/']).next().unwrap_or(s).split(" & ").next().unwrap_or(s).trim()
}

fn empty_track() -> Track {
    Track { video_id: String::new(), title: String::new(), artist: String::new(), album: None, duration: None, thumbnail: None }
}

fn song_item(t: Track) -> HomeItem {
    HomeItem {
        kind: "song".into(),
        title: t.title.clone(),
        subtitle: t.artist.clone(),
        thumbnail: t.thumbnail.clone(),
        video_id: Some(t.video_id.clone()),
        playlist_id: None,
        browse_id: None,
        track: Some(t),
    }
}

/// Personal shelves for the home screen built from local history and Last.fm.
///
/// Building them takes a YouTube radio plus a search for every Last.fm suggestion, so the
/// result is kept for an hour per last-played song: opening Home again costs nothing until you
/// play something new.
pub async fn discover(state: &AppState) -> anyhow::Result<Vec<Shelf>> {
    let recent = state.db.recent_video_ids(50).await?;
    let profile = state.db.active_profile().await;
    let lastfm = state.lastfm.session().await.is_some() as u8;
    let seed = recent.first().cloned().unwrap_or_default();
    // "yt:" prefix so the YouTube log shows these hits as "cache discover".
    let key = format!("yt:{profile}:{lastfm}:discover:{seed}");
    crate::services::cache::cached(&state.db, &key, crate::services::cache::HOUR, false, || build_discover(state, recent)).await
}

async fn build_discover(state: &AppState, recent: Vec<String>) -> anyhow::Result<Vec<Shelf>> {
    let exclude: HashSet<String> = recent.iter().cloned().collect();
    let mut shelves = Vec::new();

    if let Some(last) = recent.first() {
        if let Some(seed) = state.db.get_track(last).await? {
            let recs = for_track(state, &seed, 20, &exclude).await?;
            if !recs.is_empty() {
                shelves.push(Shelf {
                    title: format!("Because you played {}", seed.title),
                    items: recs.into_iter().map(song_item).collect(),
                });
            }
        }
    }

    // Personal Last.fm picks reflect your real listening, so they're hidden in the fresh profile.
    if state.db.active_profile().await == crate::services::profile::MAIN && state.lastfm.session().await.is_some() {
        let top = state.lastfm.top_tracks(6).await.unwrap_or_default();
        let mut pairs = Vec::new();
        for t in top {
            if let Ok(sim) = state.lastfm.similar_tracks(&t.artist, &t.title, 8).await {
                pairs.extend(sim.into_iter().take(4).map(|s| (s.artist, s.title)));
            }
        }
        let mut tracks = resolve_many(state, pairs).await;
        tracks.retain(|t| !exclude.contains(&t.video_id));
        crate::services::ytmusic::dedupe(&mut tracks);
        if !tracks.is_empty() {
            shelves.push(Shelf { title: "Last.fm picks for you".into(), items: tracks.into_iter().map(song_item).collect() });
        }
    }
    Ok(shelves)
}
