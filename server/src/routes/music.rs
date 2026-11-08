use std::collections::HashSet;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{ApiResult, AppError},
    models::{ArtistPage, GridPage, SearchResults, Shelf, Track},
    services::{
        matching, plex, recommend, spotify,
        ytmusic::{HeaderInfo, HomePage, TrackPage},
    },
    state::SharedState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeResponse {
    pub shelves: Vec<Shelf>,
    /// Pass to `/home/more` for the next shelves; absent at the end of the feed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
}

#[derive(Deserialize)]
pub struct RefreshQuery {
    #[serde(default)]
    refresh: Option<String>,
}

pub async fn home(State(st): State<SharedState>, Query(q): Query<RefreshQuery>) -> ApiResult<HomeResponse> {
    home_response(&st, st.yt.home(q.refresh.is_some()).await?).await
}

#[derive(Deserialize)]
pub struct MoreQuery {
    token: String,
}

/// The next shelves of the home feed (infinite scroll).
pub async fn home_more(State(st): State<SharedState>, Query(q): Query<MoreQuery>) -> ApiResult<HomeResponse> {
    if !q.token.contains('|') || q.token.len() > 8192 {
        return Err(AppError::bad("invalid token"));
    }
    home_response(&st, st.yt.home_more(&q.token).await?).await
}

async fn home_response(st: &SharedState, page: HomePage) -> ApiResult<HomeResponse> {
    let tracks: Vec<Track> = page.shelves.iter().flat_map(|s| s.items.iter().filter_map(|i| i.track.clone())).collect();
    st.db.upsert_tracks(tracks).await?;
    Ok(Json(HomeResponse { shelves: page.shelves, continuation: page.continuation }))
}

pub async fn discover(State(st): State<SharedState>) -> ApiResult<HomeResponse> {
    Ok(Json(HomeResponse { shelves: recommend::discover(&st).await?, continuation: None }))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn search(State(st): State<SharedState>, Query(q): Query<SearchQuery>) -> ApiResult<Vec<Track>> {
    if q.q.trim().is_empty() {
        return Ok(Json(vec![]));
    }
    let tracks = st.yt.search_songs(&q.q, q.limit.unwrap_or(30)).await?;
    st.db.upsert_tracks(tracks.clone()).await?;
    Ok(Json(tracks))
}

pub async fn radio(State(st): State<SharedState>, Path(video_id): Path<String>) -> ApiResult<Vec<Track>> {
    if !plex::is_plex_id(&video_id) && !spotify::is_spotify_id(&video_id) {
        let tracks = st.yt.watch_playlist(Some(&video_id), None, 50).await?;
        st.db.upsert_tracks(tracks.clone()).await?;
        return Ok(Json(tracks));
    }
    // A Plex-only or Spotify song: radio from its YouTube equivalent, led by the song itself.
    let seed = track_or_fetch(&st, &video_id).await?;
    let Some(id) = recommend::youtube_seed(&st, &seed).await else { return Ok(Json(vec![seed])) };
    let mut tracks = st.yt.watch_playlist(Some(&id), None, 50).await?;
    st.db.upsert_tracks(tracks.clone()).await?;
    tracks.retain(|t| t.video_id != id);
    tracks.insert(0, seed);
    Ok(Json(tracks))
}

pub async fn playlist(State(st): State<SharedState>, Path(id): Path<String>) -> ApiResult<Vec<Track>> {
    // A podcast: just its newest episodes (one YouTube call); its page loads more as you scroll.
    let tracks = if id.starts_with("MPSP") {
        st.yt.podcast_episodes(&id).await?.tracks
    } else {
        st.yt.playlist_tracks(&id, 500).await?
    };
    st.db.upsert_tracks(tracks.clone()).await?;
    Ok(Json(tracks))
}

/// A podcast's newest episodes and the token for more: one YouTube call per page.
pub async fn podcast(State(st): State<SharedState>, Path(id): Path<String>) -> ApiResult<TrackPage> {
    valid_browse_id(&id)?;
    let page = st.yt.podcast_episodes(&id).await?;
    st.db.upsert_tracks(page.tracks.clone()).await?;
    Ok(Json(page))
}

/// The next page of a podcast's episodes (`token` from the page before).
pub async fn podcast_more(State(st): State<SharedState>, Query(q): Query<MoreQuery>) -> ApiResult<TrackPage> {
    if q.token.is_empty() || q.token.len() > 8192 {
        return Err(AppError::bad("invalid token"));
    }
    let page = st.yt.episodes_more(&q.token).await?;
    st.db.upsert_tracks(page.tracks.clone()).await?;
    Ok(Json(page))
}

/// A playlist's or album's name, owner/artist and cover.
pub async fn playlist_info(State(st): State<SharedState>, Path(id): Path<String>) -> ApiResult<HeaderInfo> {
    valid_browse_id(&id)?;
    Ok(Json(st.yt.playlist_info(&id).await?))
}

#[derive(Deserialize)]
pub struct RecQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// Blended YouTube radio + Last.fm recommendations seeded by a track.
pub async fn recommendations(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Query(q): Query<RecQuery>,
) -> ApiResult<Vec<Track>> {
    let seed = track_or_fetch(&st, &video_id).await?;
    let exclude: HashSet<String> = st.db.recent_video_ids(30).await?.into_iter().collect();
    Ok(Json(recommend::for_track(&st, &seed, q.limit.unwrap_or(25), &exclude).await?))
}

pub async fn track(State(st): State<SharedState>, Path(video_id): Path<String>) -> ApiResult<Track> {
    Ok(Json(track_or_fetch(&st, &video_id).await?))
}

pub async fn track_or_fetch(st: &SharedState, video_id: &str) -> Result<Track, AppError> {
    if let Some(t) = st.db.get_track(video_id).await? {
        return Ok(t);
    }
    // Spotify songs are stored when their playlist is imported; there's nothing to ask YouTube.
    if spotify::is_spotify_id(video_id) {
        return Err(AppError::NotFound);
    }
    let t = match plex::rating_key(video_id) {
        Some(rk) => st.plex.track_by_key(rk).await?.ok_or(AppError::NotFound)?.to_track(),
        None => st.yt.song_info(video_id).await?,
    };
    st.db.upsert_tracks(vec![t.clone()]).await?;
    Ok(t)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    played_at: i64,
    source: String,
    scrobbled: bool,
    track: Track,
}

pub async fn history(State(st): State<SharedState>) -> ApiResult<Vec<HistoryEntry>> {
    let profile = st.db.active_profile().await;
    let rows = st
        .db
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT h.played_at, h.source, h.scrobbled, t.video_id, t.title, t.artist, t.album, t.duration, t.thumbnail
                 FROM history h JOIN tracks t ON t.video_id = h.video_id
                 WHERE h.profile = ?1
                 ORDER BY h.played_at DESC LIMIT 200",
            )?;
            let rows = stmt.query_map([profile], |r| {
                Ok(HistoryEntry {
                    played_at: r.get(0)?,
                    source: r.get(1)?,
                    scrobbled: r.get::<_, i64>(2)? == 1,
                    track: Track {
                        video_id: r.get(3)?,
                        title: r.get(4)?,
                        artist: r.get(5)?,
                        album: r.get(6)?,
                        duration: r.get(7)?,
                        thumbnail: r.get(8)?,
                    },
                })
            })?;
            rows.collect()
        })
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct SearchAllQuery {
    q: String,
    /// artists | albums | songs (default: everything)
    #[serde(default)]
    filter: Option<String>,
}

/// Search across songs, artists, albums, videos and playlists (or one kind with `filter`).
pub async fn search_all(State(st): State<SharedState>, Query(q): Query<SearchAllQuery>) -> ApiResult<SearchResults> {
    if q.q.trim().is_empty() {
        return Ok(Json(SearchResults::default()));
    }
    let res = match q.filter.as_deref() {
        Some("songs") => st.yt.search_songs(&q.q, 30).await.map(|songs| SearchResults { songs, ..Default::default() }),
        f => st.yt.search_all(&q.q, f).await,
    };
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("youtube search failed, searching plex and the audio cache: {e:#}");
            return Ok(Json(offline_search(&st, &q.q, &format!("{e:#}")).await));
        }
    };
    let mut tracks = res.songs.clone();
    tracks.extend(res.top.iter().filter_map(|t| t.track.clone()));
    st.db.upsert_tracks(tracks).await?;
    Ok(Json(res))
}

/// Songs that play without YouTube: Plex tracks, and songs with an MP3 in the audio cache.
async fn offline_search(st: &SharedState, query: &str, error: &str) -> SearchResults {
    let words: Vec<String> = matching::normalize(query).split_whitespace().map(String::from).collect();
    let matches = |t: &Track| {
        let hay = matching::normalize(&format!("{} {} {}", t.artist, t.title, t.album.as_deref().unwrap_or_default()));
        words.iter().all(|w| hay.contains(w.as_str()))
    };

    let mut songs = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if st.plex.server().await.is_some() {
        match st.plex.search(query).await {
            Ok(hits) => {
                // Plex's search is loose (it matches on parts of words and on other fields); keep
                // what contains every word typed, like the cache results.
                for h in hits {
                    let t = h.to_track();
                    let with_album_artist = Track { artist: format!("{} {}", t.artist, h.album_artist), ..t.clone() };
                    if matches(&with_album_artist) {
                        seen.insert(format!("{}|{}", matching::normalize(&t.artist), matching::normalize(&t.title)));
                        songs.push(t);
                    }
                }
            }
            Err(e) => tracing::warn!("plex search failed too: {e:#}"),
        }
    }
    for vid in st.audio_cache.fresh_ids() {
        let Ok(Some(t)) = st.db.get_track(&vid).await else { continue };
        // Plex has it already: Plex plays better.
        if matches(&t) && seen.insert(format!("{}|{}", matching::normalize(&t.artist), matching::normalize(&t.title))) {
            songs.push(t);
        }
    }
    let _ = st.db.upsert_tracks(songs.clone()).await;
    let brief = error.lines().next().unwrap_or(error).chars().take(160).collect::<String>();
    SearchResults { songs, fallback: Some(brief), ..Default::default() }
}

fn valid_browse_id(id: &str) -> Result<(), AppError> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(AppError::bad("invalid id"));
    }
    Ok(())
}

pub async fn artist(State(st): State<SharedState>, Path(id): Path<String>) -> ApiResult<ArtistPage> {
    valid_browse_id(&id)?;
    let page = st.yt.artist(&id).await?;
    st.db.upsert_tracks(page.top_songs.clone()).await?;
    Ok(Json(page))
}

#[derive(Deserialize)]
pub struct ArtistLookup {
    name: String,
}

/// The artist page id for a name: `{ "id": "UC…" }` or `{ "id": null }`.
pub async fn artist_lookup(State(st): State<SharedState>, Query(q): Query<ArtistLookup>) -> ApiResult<serde_json::Value> {
    let name = q.name.trim();
    if name.is_empty() {
        return Err(AppError::bad("name required"));
    }
    Ok(Json(serde_json::json!({ "id": st.yt.artist_id_for(name).await? })))
}

#[derive(Deserialize)]
pub struct GridQuery {
    id: String,
    #[serde(default)]
    params: Option<String>,
}

/// A grid page, e.g. an artist's full discography (the "See all" of Albums / Singles).
pub async fn browse_grid(State(st): State<SharedState>, Query(q): Query<GridQuery>) -> ApiResult<GridPage> {
    valid_browse_id(&q.id)?;
    Ok(Json(st.yt.browse_grid(&q.id, q.params.as_deref()).await?))
}

// ---- a song's YouTube match ---------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeMatch {
    /// `self`: a YouTube song playing from its own video (`track`); `picked`: a YouTube song playing
    /// from another video you picked (`track`); `matched`: a song from elsewhere playing from
    /// `track`; `none`: looked up and nothing found; `unknown`: not looked up yet (it is when it
    /// first plays); `plex`: a Plex-only song.
    kind: &'static str,
    track: Option<Track>,
    /// Nothing can play the song right now.
    missing: bool,
}

async fn youtube_match_of(st: &SharedState, video_id: &str) -> Result<YoutubeMatch, AppError> {
    let missing = crate::services::missing::list(st).await.iter().any(|m| m == video_id);
    if plex::is_plex_id(video_id) {
        return Ok(YoutubeMatch { kind: "plex", track: None, missing });
    }
    if !spotify::is_spotify_id(video_id) {
        if let Some(target) = crate::services::resolver::video_override(&st.db, video_id).await? {
            return Ok(YoutubeMatch { kind: "picked", track: st.db.get_track(&target).await?, missing });
        }
        return Ok(YoutubeMatch { kind: "self", track: st.db.get_track(video_id).await?, missing });
    }
    let t = track_or_fetch(st, video_id).await?;
    Ok(match crate::services::resolver::remembered(&st.db, &t.artist, &t.title).await? {
        Some(Some(id)) => YoutubeMatch { kind: "matched", track: st.db.get_track(&id).await?, missing },
        Some(None) => YoutubeMatch { kind: "none", track: None, missing },
        None => YoutubeMatch { kind: "unknown", track: None, missing },
    })
}

/// Which YouTube video a song plays from (for songs from other services).
pub async fn youtube_match(State(st): State<SharedState>, Path(video_id): Path<String>) -> ApiResult<YoutubeMatch> {
    Ok(Json(youtube_match_of(&st, &video_id).await?))
}

#[derive(Deserialize)]
pub struct YoutubePick {
    track: Track,
}

/// Play a song from this YouTube video from now on (a YouTube song: instead of its own video).
pub async fn set_youtube_match(
    State(st): State<SharedState>,
    Path(video_id): Path<String>,
    Json(b): Json<YoutubePick>,
) -> ApiResult<YoutubeMatch> {
    if plex::is_plex_id(&video_id) {
        return Err(AppError::bad("This song is only in Plex"));
    }
    if spotify::is_spotify_id(&b.track.video_id) || plex::is_plex_id(&b.track.video_id) {
        return Err(AppError::bad("Pick a YouTube video"));
    }
    st.db.upsert_tracks(vec![b.track.clone()]).await?;
    if spotify::is_spotify_id(&video_id) {
        let t = track_or_fetch(&st, &video_id).await?;
        crate::services::resolver::remember(&st.db, &t.artist, &t.title, &b.track.video_id).await?;
    } else {
        crate::services::resolver::set_video_override(&st.db, &video_id, Some(&b.track.video_id)).await?;
    }
    crate::services::missing::clear(&st, &video_id).await;
    Ok(Json(youtube_match_of(&st, &video_id).await?))
}

/// Forget a song's YouTube match and look again now (also past an earlier "nothing found"). A
/// YouTube song goes back to playing from its own video.
pub async fn retry_youtube_match(State(st): State<SharedState>, Path(video_id): Path<String>) -> ApiResult<YoutubeMatch> {
    if plex::is_plex_id(&video_id) {
        return Err(AppError::bad("This song is only in Plex"));
    }
    if !spotify::is_spotify_id(&video_id) {
        crate::services::resolver::set_video_override(&st.db, &video_id, None).await?;
        crate::services::missing::clear(&st, &video_id).await;
        return Ok(Json(youtube_match_of(&st, &video_id).await?));
    }
    let t = track_or_fetch(&st, &video_id).await?;
    let found = crate::services::resolver::resolve_opts(&st.yt, &st.db, &t.artist, &t.title, t.duration, true).await?;
    match found {
        Some(_) => crate::services::missing::clear(&st, &video_id).await,
        None => crate::services::missing::mark(&st, &video_id, "no match on YouTube").await,
    }
    Ok(Json(youtube_match_of(&st, &video_id).await?))
}

/// Songs nothing can play right now (shown with a mark on their artwork).
pub async fn missing(State(st): State<SharedState>) -> ApiResult<Vec<String>> {
    Ok(Json(crate::services::missing::list(&st).await))
}

/// YouTube videos only (for picking a song's audio by hand: uploads that aren't in the song catalogue).
pub async fn search_videos(State(st): State<SharedState>, Query(q): Query<SearchQuery>) -> ApiResult<Vec<Track>> {
    if q.q.trim().is_empty() {
        return Ok(Json(vec![]));
    }
    let tracks = st.yt.search_videos(&q.q, q.limit.unwrap_or(20)).await?;
    st.db.upsert_tracks(tracks.clone()).await?;
    Ok(Json(tracks))
}
