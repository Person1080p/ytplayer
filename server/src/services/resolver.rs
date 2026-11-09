//! Map (artist, title) pairs from other services onto YouTube Music video ids (or, with YouTube
//! off, onto the library sources' copies).

use rusqlite::{params, OptionalExtension};

use crate::{
    db::Db,
    models::{now, Track},
    services::{matching, sources, ytmusic::YtMusic},
};

const THRESHOLD: f32 = 0.62;
/// Videos (the fallback when no song matches) need a closer match.
const VIDEO_THRESHOLD: f32 = 0.8;
const NEGATIVE_TTL: i64 = 60 * 60 * 24 * 7;

/// Remember a user's manual choice so future imports of the same song use it.
pub async fn remember(db: &Db, artist: &str, title: &str, video_id: &str) -> anyhow::Result<()> {
    let key = format!("{}|{}", matching::primary_artist(artist), matching::normalize(title));
    let vid = video_id.to_string();
    db.call(move |c| {
        c.execute(
            "INSERT OR REPLACE INTO lookup_cache(key, video_id, created_at) VALUES(?1,?2,?3)",
            params![key, vid, now()],
        )
        .map(|_| ())
    })
    .await
}

/// The video a YouTube song plays from instead of its own, when one was picked by hand.
pub async fn video_override(db: &Db, video_id: &str) -> anyhow::Result<Option<String>> {
    let v = video_id.to_string();
    db.call(move |c| c.query_row("SELECT target_id FROM video_overrides WHERE video_id=?1", [v], |r| r.get(0)).optional())
        .await
}

/// Play a YouTube song from another video (`None`, or the song's own id, goes back to its own).
pub async fn set_video_override(db: &Db, video_id: &str, target: Option<&str>) -> anyhow::Result<()> {
    let (v, t) = (video_id.to_string(), target.filter(|t| *t != video_id).map(str::to_string));
    db.call(move |c| {
        match t {
            Some(t) => c.execute(
                "INSERT OR REPLACE INTO video_overrides(video_id, target_id, at) VALUES(?1,?2,?3)",
                params![v, t, now()],
            ),
            None => c.execute("DELETE FROM video_overrides WHERE video_id=?1", [v]),
        }
        .map(|_| ())
    })
    .await
}

fn key(artist: &str, title: &str) -> String {
    format!("{}|{}", matching::primary_artist(artist), matching::normalize(title))
}

// ---- songs matched when they play -------------------------------------------------------------

const SONG_PREFIX: &str = "song_";

/// The id of a song added without a YouTube video (a library import that doesn't look songs up):
/// stable per artist and title, so the same song imported twice is one song.
pub fn song_id(artist: &str, title: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(key(artist, title).as_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{SONG_PREFIX}{hex}")
}

pub fn is_song_id(video_id: &str) -> bool {
    video_id
        .strip_prefix(SONG_PREFIX)
        .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A song from the music folder on a device running the web app (`local_<16 hex>`, made by the
/// app from its artist and title). The server can't reach that folder: elsewhere such a song plays
/// like an imported one, matched by its tags.
pub fn is_local_id(video_id: &str) -> bool {
    video_id
        .strip_prefix("local_")
        .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A song with no YouTube video of its own (a Spotify playlist song, or a library import added
/// without looking it up): matched by title, artist and length when first needed.
pub fn is_deferred_id(video_id: &str) -> bool {
    crate::services::spotify::is_spotify_id(video_id) || is_song_id(video_id) || is_local_id(video_id)
}

/// The YouTube video a deferred song plays from: looked up the first time it's needed (when it
/// plays, or radio starts from it), then remembered. None when YouTube has nothing convincing
/// (or is off).
pub async fn deferred_youtube_id(st: &crate::state::AppState, video_id: &str) -> anyhow::Result<Option<String>> {
    use anyhow::Context;
    if !sources::youtube_enabled(&st.db).await {
        return Ok(None);
    }
    let mut t = st.db.get_track(video_id).await?.context("unknown song")?;
    let found = resolve(&st.yt, &st.db, &t.artist, &t.title, t.duration).await?;
    // An imported or device-folder song has no art of its own on the server: use its video's
    // (Spotify songs get Spotify's).
    if let Some(f) = &found {
        if is_song_id(video_id) || is_local_id(video_id) {
            if t.thumbnail.is_none() && f.thumbnail.is_some() {
                t.thumbnail = f.thumbnail.clone();
                st.db.upsert_tracks(vec![t]).await?;
            }
            // Its library entry is matched now (it keeps the song's id, which queues refer to).
            let id = video_id.to_string();
            st.db
                .call(move |c| c.execute("UPDATE liked SET status='matched' WHERE video_id=?1 AND status='deferred'", [id]))
                .await?;
        }
    }
    Ok(found.map(|t| t.video_id))
}

/// The remembered match for a song: `Some(Some(id))` matched, `Some(None)` looked up and nothing
/// found, `None` never looked up.
pub async fn remembered(db: &Db, artist: &str, title: &str) -> anyhow::Result<Option<Option<String>>> {
    let k = key(artist, title);
    db.call(move |c| c.query_row("SELECT video_id FROM lookup_cache WHERE key=?1", [k], |r| r.get(0)).optional()).await
}

/// A song from an import: its YouTube match, or with YouTube off its copy in the library
/// sources (in priority order), as a song under its source-only id (`plex_…`, `jf_…`, `local_…`)
/// so the library entry plays from that file. `force` ignores a cached "no match" on YouTube.
pub async fn resolve_song(
    st: &crate::state::AppState,
    artist: &str,
    title: &str,
    album: Option<&str>,
    duration: Option<u32>,
    force: bool,
) -> anyhow::Result<Option<Track>> {
    if sources::youtube_enabled(&st.db).await {
        return resolve_opts(&st.yt, &st.db, artist, title, duration, force).await;
    }
    let want = Track {
        video_id: song_id(artist, title),
        title: title.to_string(),
        artist: artist.to_string(),
        album: album.map(String::from),
        duration,
        thumbnail: None,
    };
    Ok(sources::best_match(st, &want).await)
}

pub async fn resolve(
    yt: &YtMusic,
    db: &Db,
    artist: &str,
    title: &str,
    duration: Option<u32>,
) -> anyhow::Result<Option<Track>> {
    resolve_opts(yt, db, artist, title, duration, false).await
}

/// `force` ignores a cached "no match" (used when retrying unmatched library tracks).
pub async fn resolve_opts(
    yt: &YtMusic,
    db: &Db,
    artist: &str,
    title: &str,
    duration: Option<u32>,
    force: bool,
) -> anyhow::Result<Option<Track>> {
    let key = format!("{}|{}", matching::primary_artist(artist), matching::normalize(title));
    let k = key.clone();
    let cached: Option<(Option<String>, i64)> = db
        .call(move |c| {
            c.query_row("SELECT video_id, created_at FROM lookup_cache WHERE key=?1", [k], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
        })
        .await?;
    match cached {
        Some((Some(vid), _)) => {
            if let Some(t) = db.get_track(&vid).await? {
                return Ok(Some(t));
            }
        }
        Some((None, at)) if !force && now() - at < NEGATIVE_TTL => return Ok(None),
        _ => {}
    }
    if !sources::youtube_enabled(db).await {
        anyhow::bail!("{}", sources::YOUTUBE_OFF);
    }

    let pick = |results: Vec<Track>| {
        results
            .into_iter()
            .map(|t| (matching::score(title, artist, duration, &t.title, &t.artist, t.duration), t))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .filter(|(s, _)| *s >= THRESHOLD)
            .map(|(_, t)| t)
    };
    let mut best = pick(yt.search_songs(&format!("{artist} {title}"), 8).await?);
    if best.is_none() {
        // Second try with decorations stripped ("Song - 2011 Remaster (feat. X)" → "song").
        let simple = format!("{} {}", matching::primary_artist(artist), matching::normalize(title));
        if simple.trim() != format!("{artist} {title}").to_lowercase().trim() {
            best = pick(yt.search_songs(&simple, 8).await?);
        }
    }
    if best.is_none() {
        // Not in YouTube Music's song catalogue (demos, mixes, uploads only on YouTube): videos,
        // held to a closer match, since a channel's other uploads ("How to use …") share words.
        let videos = yt.search_videos(&format!("{artist} {title}"), 10).await?;
        best = videos
            .into_iter()
            .map(|t| (matching::score(title, artist, duration, &t.title, &t.artist, t.duration), t))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .filter(|(s, _)| *s >= VIDEO_THRESHOLD)
            .map(|(_, t)| t);
    }

    if let Some(t) = &best {
        db.upsert_tracks(vec![t.clone()]).await?;
    }
    let vid = best.as_ref().map(|t| t.video_id.clone());
    db.call(move |c| {
        c.execute(
            "INSERT OR REPLACE INTO lookup_cache(key, video_id, created_at) VALUES(?1,?2,?3)",
            params![key, vid, now()],
        )
        .map(|_| ())
    })
    .await?;
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_ids() {
        let a = song_id("Daft Punk", "Get Lucky");
        assert!(is_song_id(&a) && is_deferred_id(&a));
        assert_eq!(a, song_id("Daft Punk", "Get Lucky"));
        assert_ne!(a, song_id("Daft Punk", "One More Time"));
        assert!(!is_song_id("dQw4w9WgXcQ") && !is_deferred_id("dQw4w9WgXcQ"));
        assert!(!is_song_id("song_xyz"));
        assert!(is_deferred_id("spotify_3OYqNt713cxVT3R0idEfZp"));
    }
}
