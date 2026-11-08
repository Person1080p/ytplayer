//! Songs with no audio anywhere: not in Plex, nothing found on YouTube (a Spotify song with no
//! match), or a YouTube video that's gone (removed, private, blocked). Shown with a mark on their
//! artwork; cleared when they play again or a match is picked.

use std::{collections::HashSet, sync::Mutex};

use rusqlite::params;

use crate::{
    models::now,
    state::{AppState, ServerEvent},
};

/// The ids in the table, kept in memory: every stream request checks it.
static MISSING: std::sync::LazyLock<Mutex<Option<HashSet<String>>>> = std::sync::LazyLock::new(|| Mutex::new(None));

async fn ensure_loaded(st: &AppState) {
    if MISSING.lock().map(|m| m.is_some()).unwrap_or(true) {
        return;
    }
    let ids: Vec<String> = st
        .db
        .call(|c| {
            let mut stmt = c.prepare("SELECT video_id FROM missing_audio")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .await
        .unwrap_or_default();
    if let Ok(mut m) = MISSING.lock() {
        m.get_or_insert_with(|| ids.into_iter().collect());
    }
}

/// Songs currently without audio.
pub async fn list(st: &AppState) -> Vec<String> {
    ensure_loaded(st).await;
    MISSING.lock().ok().and_then(|m| m.as_ref().map(|s| s.iter().cloned().collect())).unwrap_or_default()
}

/// Nothing can play `video_id` (`reason` for the log).
pub async fn mark(st: &AppState, video_id: &str, reason: &str) {
    ensure_loaded(st).await;
    let new = MISSING.lock().ok().and_then(|mut m| m.as_mut().map(|s| s.insert(video_id.to_string()))).unwrap_or(false);
    if !new {
        return;
    }
    tracing::info!("no audio for {video_id}: {reason}");
    let (v, r) = (video_id.to_string(), reason.chars().take(300).collect::<String>());
    let _ = st
        .db
        .call(move |c| c.execute("INSERT OR REPLACE INTO missing_audio(video_id, reason, at) VALUES(?1,?2,?3)", params![v, r, now()]))
        .await;
    st.emit(ServerEvent::Missing { video_id: video_id.to_string(), missing: true });
}

/// `video_id` has audio (it played, or a match was picked).
pub async fn clear(st: &AppState, video_id: &str) {
    ensure_loaded(st).await;
    let was = MISSING.lock().ok().and_then(|mut m| m.as_mut().map(|s| s.remove(video_id))).unwrap_or(false);
    if !was {
        return;
    }
    let v = video_id.to_string();
    let _ = st.db.call(move |c| c.execute("DELETE FROM missing_audio WHERE video_id=?1", [v])).await;
    st.emit(ServerEvent::Missing { video_id: video_id.to_string(), missing: false });
}

/// Whether a yt-dlp error means the video itself can't be played (as opposed to a passing
/// network or rate-limit problem).
pub fn video_gone(error: &str) -> bool {
    let e = error.to_lowercase();
    [
        "video unavailable",
        "private video",
        "this video is not available",
        "has been removed",
        "account associated with this video has been terminated",
        "copyright claim",
        "not available in your country",
        "members-only",
        "join this channel",
        "this video has been removed",
        "no longer available",
        "video is unavailable",
        "is not a valid url",
        "incomplete youtube id",
    ]
    .iter()
    .any(|m| e.contains(m))
}

#[cfg(test)]
mod tests {
    #[test]
    fn gone_errors() {
        assert!(super::video_gone("ERROR: [youtube] abc: Video unavailable. This video has been removed by the uploader"));
        assert!(super::video_gone("ERROR: [youtube] abc: Private video. Sign in if you've been granted access"));
        assert!(!super::video_gone("HTTP Error 429: Too Many Requests"));
        assert!(!super::video_gone("timed out"));
    }
}
