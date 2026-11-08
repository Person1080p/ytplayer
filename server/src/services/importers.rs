//! Importing liked tracks from YouTube Music, Deezer and CSV/text exports.

use anyhow::{bail, Context};
use rusqlite::params;
use serde_json::Value;

use std::collections::{HashMap, HashSet};

use crate::{
    models::{now, ImportItem},
    services::{lastfm, matching, resolver},
    state::{ServerEvent, SharedState},
};

// ---- job runner -------------------------------------------------------------

/// Create an import job and run it in the background. `fetch` produces the provider's items.
pub async fn start<F>(state: SharedState, provider: &str, fetch: F) -> anyhow::Result<i64>
where
    F: std::future::Future<Output = anyhow::Result<Vec<ImportItem>>> + Send + 'static,
{
    let p = provider.to_string();
    let job_id = state
        .db
        .call(move |c| {
            c.execute("INSERT INTO import_jobs(provider,status,started_at) VALUES(?1,'running',?2)", params![p, now()])?;
            Ok(c.last_insert_rowid())
        })
        .await?;

    let provider = provider.to_string();
    tokio::spawn(crate::services::ytlog::with_origin(format!("import:{provider}"), async move {
        let result = run(&state, job_id, &provider, fetch).await;
        let (status, err) = match result {
            Ok(()) => ("done", None),
            Err(e) => {
                tracing::error!("import {provider} failed: {e:#}");
                ("failed", Some(format!("{e:#}")))
            }
        };
        let _ = state
            .db
            .call(move |c| {
                c.execute(
                    "UPDATE import_jobs SET status=?1, error=?2, finished_at=?3 WHERE id=?4",
                    params![status, err, now(), job_id],
                )
            })
            .await;
        state.emit(ServerEvent::Import { job_id });
    }));
    Ok(job_id)
}

async fn run<F>(state: &SharedState, job_id: i64, provider: &str, fetch: F) -> anyhow::Result<()>
where
    F: std::future::Future<Output = anyhow::Result<Vec<ImportItem>>>,
{
    let items = fetch.await?;
    let total = items.len() as i64;
    state.db.call(move |c| c.execute("UPDATE import_jobs SET total=?1 WHERE id=?2", params![total, job_id])).await?;
    state.emit(ServerEvent::Import { job_id });
    process_items(state, job_id, provider, items, 0, 0).await?;
    Ok(())
}

/// Match and store items. `done0`/`matched0` are progress already made by the job.
/// Returns how many were matched.
async fn process_items(
    state: &SharedState,
    job_id: i64,
    provider: &str,
    items: Vec<ImportItem>,
    done0: i64,
    matched0: i64,
) -> anyhow::Result<i64> {
    let mut matched = matched0;
    for (i, it) in items.into_iter().enumerate() {
        // Skip re-matching rows that were already matched by a previous import.
        let (p, pid) = (provider.to_string(), it.provider_id.clone());
        let existing: Option<String> = state
            .db
            .call(move |c| {
                c.query_row("SELECT video_id FROM liked WHERE provider=?1 AND provider_id=?2", params![p, pid], |r| r.get(0))
                    .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
            })
            .await?;

        // "error" = the lookup itself failed (e.g. rate limited) — retried by "Retry unmatched".
        let mut status = "unmatched";
        let video_id = match (it.video_id.clone(), existing) {
            (Some(v), _) => {
                ensure_track(state, &v, &it).await;
                Some(v)
            }
            (None, Some(v)) => Some(v),
            (None, None) => {
                // Spacing between YouTube requests is handled by YtMusic's background pacer
                // (or, at the original pace, by this short per-song pause).
                let r = resolver::resolve(&state.yt, &state.db, &it.artist, &it.title, it.duration).await;
                tokio::time::sleep(state.yt.song_gap()).await;
                match r {
                    Ok(t) => t.map(|t| t.video_id),
                    Err(e) => {
                        tracing::warn!("lookup failed for {} - {}: {e:#}", it.artist, it.title);
                        status = "error";
                        None
                    }
                }
            }
        };
        if video_id.is_some() {
            status = "matched";
        }
        if video_id.is_some() {
            matched += 1;
        }
        let p = provider.to_string();
        let done = done0 + i as i64 + 1;
        state
            .db
            .call(move |c| {
                c.execute(
                    "INSERT INTO liked(provider,provider_id,title,artist,album,duration,video_id,status,added_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(provider,provider_id) DO UPDATE SET
                       title=excluded.title, artist=excluded.artist, album=excluded.album,
                       duration=excluded.duration,
                       -- never overwrite a match the user picked by hand
                       video_id=CASE WHEN liked.status='manual' THEN liked.video_id ELSE excluded.video_id END,
                       status=CASE WHEN liked.status='manual' THEN 'manual' ELSE excluded.status END",
                    params![
                        p,
                        it.provider_id,
                        it.title,
                        it.artist,
                        it.album,
                        it.duration,
                        video_id,
                        status,
                        now()
                    ],
                )?;
                c.execute("UPDATE import_jobs SET done=?1, matched=?2 WHERE id=?3", params![done, matched, job_id])
            })
            .await?;
        if done % 10 == 0 {
            state.emit(ServerEvent::Import { job_id });
        }
    }
    Ok(matched - matched0)
}

/// Make sure a track row exists for an item that already carries a YouTube id.
/// Uses the export's own metadata when complete, otherwise asks YouTube.
async fn ensure_track(state: &SharedState, video_id: &str, it: &ImportItem) {
    if matches!(state.db.get_track(video_id).await, Ok(Some(_))) || crate::services::plex::is_plex_id(video_id)
        || crate::services::spotify::is_spotify_id(video_id)
    {
        return;
    }
    let track = if !it.title.is_empty() && !it.artist.is_empty() {
        Some(crate::models::Track {
            video_id: video_id.to_string(),
            title: it.title.clone(),
            artist: it.artist.clone(),
            album: it.album.clone(),
            duration: it.duration,
            thumbnail: Some(format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")),
        })
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        match state.yt.song_info(video_id).await {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::debug!("song_info {video_id}: {e:#}");
                None
            }
        }
    };
    if let Some(t) = track {
        let _ = state.db.upsert_tracks(vec![t]).await;
    }
}

// ---- retry unmatched ---------------------------------------------------------------

/// Re-run matching for library entries that are unmatched or whose lookup failed,
/// ignoring cached "no match" results.
pub async fn start_rematch(state: SharedState) -> anyhow::Result<i64> {
    let job_id = state
        .db
        .call(|c| {
            c.execute("INSERT INTO import_jobs(provider,status,started_at) VALUES('rematch','running',?1)", params![now()])?;
            Ok(c.last_insert_rowid())
        })
        .await?;
    tokio::spawn(crate::services::ytlog::with_origin("import:rematch", async move {
        let res = rematch(&state, job_id).await;
        let (status, err) = match res {
            Ok(()) => ("done", None),
            Err(e) => ("failed", Some(format!("{e:#}"))),
        };
        let _ = state
            .db
            .call(move |c| {
                c.execute("UPDATE import_jobs SET status=?1, error=?2, finished_at=?3 WHERE id=?4", params![status, err, now(), job_id])
            })
            .await;
        state.emit(ServerEvent::Import { job_id });
    }));
    Ok(job_id)
}

async fn rematch(state: &SharedState, job_id: i64) -> anyhow::Result<()> {
    let rows: Vec<(i64, String, String, Option<u32>)> = state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, artist, title, duration FROM liked WHERE status IN ('unmatched','error')")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            rows.collect()
        })
        .await?;
    let total = rows.len() as i64;
    state.db.call(move |c| c.execute("UPDATE import_jobs SET total=?1 WHERE id=?2", params![total, job_id])).await?;
    let mut matched = 0i64;
    for (i, (id, artist, title, duration)) in rows.into_iter().enumerate() {
        let (vid, status) = match resolver::resolve_opts(&state.yt, &state.db, &artist, &title, duration, true).await {
            Ok(Some(t)) => (Some(t.video_id), "matched"),
            Ok(None) => (None, "unmatched"),
            Err(e) => {
                tracing::warn!("lookup failed for {artist} - {title}: {e:#}");
                (None, "error")
            }
        };
        if vid.is_some() {
            matched += 1;
        }
        let done = i as i64 + 1;
        state
            .db
            .call(move |c| {
                c.execute("UPDATE liked SET video_id=?1, status=?2 WHERE id=?3 AND status<>'manual'", params![vid, status, id])?;
                c.execute("UPDATE import_jobs SET done=?1, matched=?2 WHERE id=?3", params![done, matched, job_id])
            })
            .await?;
        if done % 10 == 0 {
            state.emit(ServerEvent::Import { job_id });
        }
        tokio::time::sleep(state.yt.song_gap()).await;
    }
    Ok(())
}

// ---- Last.fm loved tracks ---------------------------------------------------------------

/// Legacy on/off pref for the daily one-way import (still read as a fallback).
pub const LASTFM_SYNC_PREF: &str = "prefs.lastfm_sync_loves";
/// "off" | "pull" (loves → library) | "both" (library ↔ loves)
pub const LASTFM_SYNC_MODE: &str = "prefs.lastfm_sync_mode";

pub async fn lastfm_sync_mode(db: &crate::db::Db) -> String {
    match db.get_setting(LASTFM_SYNC_MODE).await.ok().flatten() {
        Some(m) if ["off", "pull", "both"].contains(&m.as_str()) => m,
        _ => if db.get_bool(LASTFM_SYNC_PREF, false).await { "pull".into() } else { "off".into() },
    }
}

/// Song identity for comparing the library with Last.fm: "primary artist|title", normalized.
pub fn love_key(artist: &str, title: &str) -> String {
    format!("{}|{}", matching::primary_artist(artist), matching::normalize(title))
}

fn lastfm_item(artist: String, title: String) -> ImportItem {
    ImportItem {
        provider_id: format!("{}|{}", artist.to_lowercase(), title.to_lowercase()),
        title,
        artist,
        album: None,
        duration: None,
        video_id: None,
    }
}

/// (id, provider, artist, title) of every library entry.
async fn library_rows(state: &SharedState) -> anyhow::Result<Vec<(i64, String, String, String)>> {
    state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, provider, artist, title FROM liked")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            rows.collect()
        })
        .await
}

/// One-way import of loved tracks (loves → library). Songs already in the library from
/// another source are skipped; Last.fm entries that were un-loved are removed.
pub async fn lastfm_loved(state: SharedState) -> anyhow::Result<Vec<ImportItem>> {
    let loved = state.lastfm.loved_tracks().await?;
    let lib = library_rows(&state).await?;
    let elsewhere: HashSet<String> =
        lib.iter().filter(|(_, p, _, _)| p != "lastfm").map(|(_, _, a, t)| love_key(a, t)).collect();
    let items: Vec<ImportItem> = loved
        .into_iter()
        .filter(|(a, t)| !elsewhere.contains(&love_key(a, t)))
        .map(|(a, t)| lastfm_item(a, t))
        .collect();
    // Mirror un-loves: drop Last.fm entries that are no longer loved.
    let keep: HashSet<String> = items.iter().map(|i| i.provider_id.clone()).collect();
    let existing: Vec<(i64, String)> = state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, provider_id FROM liked WHERE provider='lastfm'")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
        .await?;
    let gone: Vec<i64> = existing.into_iter().filter(|(_, pid)| !keep.contains(pid)).map(|(id, _)| id).collect();
    if !gone.is_empty() {
        let n = gone.len();
        delete_rows(&state, gone).await?;
        tracing::info!("removed {n} tracks no longer loved on Last.fm");
    }
    Ok(items)
}

async fn delete_rows(state: &SharedState, ids: Vec<i64>) -> anyhow::Result<()> {
    state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            for id in ids {
                tx.execute("DELETE FROM liked WHERE id=?1", [id])?;
            }
            tx.commit()
        })
        .await
}

/// What a two-way sync is going to do (computed before touching anything).
#[derive(Default, Debug, PartialEq)]
pub struct SyncPlan {
    /// Library songs to love on Last.fm: (key, artist, title)
    pub love: Vec<(String, String, String)>,
    /// Songs removed from the library since the last sync → un-love on Last.fm.
    pub unlove: Vec<(String, String, String)>,
    /// Songs un-loved on Last.fm since the last sync → remove from the library (row ids).
    pub remove: Vec<(String, Vec<i64>)>,
    /// New Last.fm loves → add to the library.
    pub add: Vec<(String, String, String)>,
    /// Keys present on both sides (kept as synced, lf_seen = 1).
    pub both: Vec<String>,
    /// Loved by us earlier but never seen in Last.fm's list (kept, lf_seen = 0).
    pub pending: Vec<String>,
}

/// Compare the library with Last.fm's loved list and the state saved by the previous sync.
pub fn plan_sync(
    lib: &[(i64, String, String, String)],
    loved: &[(String, String)],
    prev: &HashMap<String, bool>,
) -> SyncPlan {
    let mut lib_keys: HashMap<String, (Vec<i64>, String, String)> = HashMap::new();
    for (id, _, a, t) in lib {
        let e = lib_keys.entry(love_key(a, t)).or_insert_with(|| (vec![], a.clone(), t.clone()));
        e.0.push(*id);
    }
    let mut lf_keys: HashMap<String, (String, String)> = HashMap::new();
    for (a, t) in loved {
        lf_keys.entry(love_key(a, t)).or_insert_with(|| (a.clone(), t.clone()));
    }

    let mut plan = SyncPlan::default();
    // Pair near-identical spellings (Last.fm autocorrects artist names): same title, similar artist.
    let mut by_title: HashMap<String, Vec<String>> = HashMap::new();
    for k in lib_keys.keys().filter(|k| !lf_keys.contains_key(*k)) {
        let title = k.split_once('|').map(|x| x.1).unwrap_or_default().to_string();
        by_title.entry(title).or_default().push(k.clone());
    }
    let mut paired_lib: HashSet<String> = HashSet::new();
    let mut paired_lf: HashSet<String> = HashSet::new();
    for k in lf_keys.keys().filter(|k| !lib_keys.contains_key(*k)) {
        let (artist, title) = k.split_once('|').unwrap_or((k, ""));
        if title.is_empty() {
            continue;
        }
        if let Some(cands) = by_title.get_mut(title) {
            if let Some(pos) = cands.iter().position(|c| {
                let ca = c.split_once('|').map(|x| x.0).unwrap_or_default();
                matching::similarity(ca, artist) >= 0.5 || (ca.chars().count() >= 4 && artist.starts_with(&ca.chars().take(4).collect::<String>()))
            }) {
                let c = cands.remove(pos);
                plan.both.push(c.clone());
                plan.both.push(k.clone());
                paired_lib.insert(c);
                paired_lf.insert(k.clone());
            }
        }
    }

    for (k, (ids, a, t)) in &lib_keys {
        if paired_lib.contains(k) {
            continue;
        }
        if lf_keys.contains_key(k) {
            plan.both.push(k.clone());
            continue;
        }
        match prev.get(k) {
            Some(true) => plan.remove.push((k.clone(), ids.clone())),
            Some(false) => plan.pending.push(k.clone()),
            None => plan.love.push((k.clone(), lastfm::first_artist(a), t.clone())),
        }
    }
    for (k, (a, t)) in &lf_keys {
        if paired_lf.contains(k) || lib_keys.contains_key(k) {
            continue;
        }
        if prev.contains_key(k) {
            plan.unlove.push((k.clone(), a.clone(), t.clone()));
        } else {
            plan.add.push((k.clone(), a.clone(), t.clone()));
        }
    }
    plan.love.sort();
    plan.add.sort();
    plan
}

pub async fn start_lastfm_two_way(state: SharedState) -> anyhow::Result<i64> {
    let job_id = state
        .db
        .call(|c| {
            c.execute("INSERT INTO import_jobs(provider,status,started_at) VALUES('lastfm-sync','running',?1)", params![now()])?;
            Ok(c.last_insert_rowid())
        })
        .await?;
    tokio::spawn(crate::services::ytlog::with_origin("import:lastfm-sync", async move {
        let res = lastfm_two_way(&state, job_id).await;
        let (status, err) = match res {
            Ok(()) => ("done", None),
            Err(e) => {
                tracing::error!("Last.fm two-way sync failed: {e:#}");
                ("failed", Some(format!("{e:#}")))
            }
        };
        let _ = state
            .db
            .call(move |c| {
                c.execute("UPDATE import_jobs SET status=?1, error=?2, finished_at=?3 WHERE id=?4", params![status, err, now(), job_id])
            })
            .await;
        state.emit(ServerEvent::Import { job_id });
    }));
    Ok(job_id)
}

async fn set_job(state: &SharedState, job_id: i64, done: i64, matched: i64, note: String) -> anyhow::Result<()> {
    state
        .db
        .call(move |c| c.execute("UPDATE import_jobs SET done=?1, matched=?2, note=?3 WHERE id=?4", params![done, matched, note, job_id]))
        .await?;
    state.emit(ServerEvent::Import { job_id });
    Ok(())
}

/// Library ↔ Last.fm loves. New library songs get loved, new loves get added; removing a
/// song on one side (after it was synced) removes it on the other.
async fn lastfm_two_way(state: &SharedState, job_id: i64) -> anyhow::Result<()> {
    if state.lastfm.session().await.is_none() {
        bail!("Connect Last.fm in Settings first");
    }
    let loved = state.lastfm.loved_tracks().await?;
    let lib = library_rows(state).await?;
    let prev: HashMap<String, bool> = state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT key, lf_seen FROM lastfm_sync")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?;
            rows.collect()
        })
        .await?;
    let plan = plan_sync(&lib, &loved, &prev);

    // Guard against a bad response wiping things out (e.g. Last.fm returns a short list).
    let removals = plan.remove.len() + plan.unlove.len();
    if removals > 25 && removals * 4 > prev.len() {
        bail!(
            "stopped: this sync would remove {} songs from your library and un-love {} on Last.fm, which looks wrong. \
             Nothing was changed.",
            plan.remove.len(),
            plan.unlove.len()
        );
    }

    let total = (plan.love.len() + plan.unlove.len() + plan.remove.len() + plan.add.len()) as i64;
    state.db.call(move |c| c.execute("UPDATE import_jobs SET total=?1 WHERE id=?2", params![total, job_id])).await?;
    let summary = |loved: usize, unloved: usize, removed: usize, added: usize, failed: usize| {
        let mut s = format!("♥ {loved} on Last.fm · +{added} to library");
        if unloved > 0 {
            s += &format!(" · un-loved {unloved}");
        }
        if removed > 0 {
            s += &format!(" · removed {removed}");
        }
        if failed > 0 {
            s += &format!(" · {failed} failed");
        }
        s
    };

    let mut synced: Vec<(String, bool)> = plan.both.iter().map(|k| (k.clone(), true)).collect();
    synced.extend(plan.pending.iter().map(|k| (k.clone(), false)));
    let (mut done, mut loved_n, mut unloved_n, mut failed) = (0i64, 0usize, 0usize, 0usize);
    let mut streak = 0;

    // Last.fm allows a few calls per second; stay well under it.
    let pace = std::time::Duration::from_millis(250);
    for (k, a, t) in &plan.love {
        match state.lastfm.love_by_name(a, t).await {
            Ok(()) => {
                loved_n += 1;
                streak = 0;
                synced.push((k.clone(), false));
            }
            Err(e) => {
                failed += 1;
                streak += 1;
                tracing::warn!("Last.fm love {a} - {t}: {e:#}");
            }
        }
        done += 1;
        if streak >= 10 {
            save_synced(state, &synced).await?;
            bail!("Last.fm keeps refusing loves; stopped after {loved_n}");
        }
        if done % 10 == 0 {
            set_job(state, job_id, done, 0, summary(loved_n, unloved_n, 0, 0, failed)).await?;
        }
        tokio::time::sleep(pace).await;
    }
    for (k, a, t) in &plan.unlove {
        match state.lastfm.unlove_by_name(a, t).await {
            Ok(()) => unloved_n += 1,
            Err(e) => {
                failed += 1;
                // Keep it so the next sync tries again.
                synced.push((k.clone(), true));
                tracing::warn!("Last.fm unlove {a} - {t}: {e:#}");
            }
        }
        done += 1;
        tokio::time::sleep(pace).await;
    }
    let removed_n = plan.remove.len();
    delete_rows(state, plan.remove.iter().flat_map(|(_, ids)| ids.iter().copied()).collect()).await?;
    done += removed_n as i64;
    set_job(state, job_id, done, 0, summary(loved_n, unloved_n, removed_n, 0, failed)).await?;

    // Save before the (slow) YouTube matching, so an interruption doesn't re-love everything.
    save_synced(state, &synced).await?;

    let added_n = plan.add.len();
    let items: Vec<ImportItem> = plan.add.iter().map(|(_, a, t)| lastfm_item(a.clone(), t.clone())).collect();
    let matched = process_items(state, job_id, "lastfm", items, done, 0).await?;
    synced.extend(plan.add.iter().map(|(k, _, _)| (k.clone(), true)));
    save_synced(state, &synced).await?;
    set_job(state, job_id, done + added_n as i64, matched, summary(loved_n, unloved_n, removed_n, added_n, failed)).await?;
    tracing::info!("Last.fm sync: {}", summary(loved_n, unloved_n, removed_n, added_n, failed));
    Ok(())
}

async fn save_synced(state: &SharedState, synced: &[(String, bool)]) -> anyhow::Result<()> {
    let rows = synced.to_vec();
    state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM lastfm_sync", [])?;
            for (k, seen) in rows {
                tx.execute(
                    "INSERT INTO lastfm_sync(key, lf_seen) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET lf_seen = MAX(lf_seen, excluded.lf_seen)",
                    params![k, seen],
                )?;
            }
            tx.commit()
        })
        .await
}

/// Daily Last.fm loves sync, in whichever mode is selected.
pub fn spawn_lastfm_sync(state: SharedState) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        loop {
            if state.lastfm.session().await.is_some() {
                let r = match lastfm_sync_mode(&state.db).await.as_str() {
                    "pull" => start(state.clone(), "lastfm", lastfm_loved(state.clone())).await.map(|_| ()),
                    "both" => start_lastfm_two_way(state.clone()).await.map(|_| ()),
                    _ => Ok(()),
                };
                if let Err(e) = r {
                    tracing::warn!("Last.fm loves sync: {e:#}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

// ---- YouTube Music ------------------------------------------------------------

pub async fn ytmusic_liked(state: SharedState) -> anyhow::Result<Vec<ImportItem>> {
    let tracks = state.yt.liked_songs(10_000).await?;
    state.db.upsert_tracks(tracks.clone()).await?;
    Ok(tracks
        .into_iter()
        .map(|t| ImportItem {
            provider_id: t.video_id.clone(),
            title: t.title,
            artist: t.artist,
            album: t.album,
            duration: t.duration,
            video_id: Some(t.video_id),
        })
        .collect())
}


pub async fn deezer_favorites(http: reqwest::Client, user_id: String, token: Option<String>) -> anyhow::Result<Vec<ImportItem>> {
    let mut out = Vec::new();
    let mut url = Some(format!("https://api.deezer.com/user/{}/tracks?limit=100", urlencoding::encode(&user_id)));
    while let Some(u) = url.take() {
        let mut req = http.get(&u);
        if let Some(t) = &token {
            req = req.query(&[("access_token", t)]);
        }
        let v: Value = req.send().await?.error_for_status()?.json().await?;
        if let Some(err) = v.get("error") {
            bail!("deezer: {}", err["message"].as_str().unwrap_or("unknown error"));
        }
        for t in v["data"].as_array().into_iter().flatten() {
            out.push(ImportItem {
                provider_id: t["id"].as_i64().map(|i| i.to_string()).unwrap_or_default(),
                title: t["title"].as_str().unwrap_or_default().to_string(),
                artist: t["artist"]["name"].as_str().unwrap_or_default().to_string(),
                album: t["album"]["title"].as_str().map(String::from),
                duration: t["duration"].as_u64().map(|d| d as u32),
                video_id: None,
            });
        }
        url = v["next"].as_str().map(String::from);
    }
    Ok(out)
}

// ---- CSV / plain text ---------------------------------------------------------

/// Accepts CSV exports (Exportify, TuneMyMusic, Soundiiz, Deezer exports, ...) or
/// plain "Artist - Title" lines.
pub fn parse_text(content: &str) -> anyhow::Result<Vec<ImportItem>> {
    let first = content.lines().next().unwrap_or_default().to_lowercase();
    let looks_csv = first.contains(',') && (first.contains("title") || first.contains("track") || first.contains("name"));
    if !looks_csv {
        return Ok(content
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(|l| {
                let (artist, title) = l.split_once(" - ").or_else(|| l.split_once(" – "))?;
                Some(ImportItem {
                    provider_id: format!("{}|{}", artist.trim().to_lowercase(), title.trim().to_lowercase()),
                    title: title.trim().into(),
                    artist: artist.trim().into(),
                    album: None,
                    duration: None,
                    video_id: None,
                })
            })
            .collect());
    }

    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(content.as_bytes());
    let headers: Vec<String> = rdr.headers()?.iter().map(|h| h.trim().to_lowercase()).collect();
    let find = |names: &[&str]| headers.iter().position(|h| names.iter().any(|n| h == n));
    let title_i = find(&["track name", "title", "track", "name", "song", "track title"]).context("no title column in CSV")?;
    let artist_i = find(&["artist name(s)", "artist name", "artist", "artists", "artist(s)"]).context("no artist column in CSV")?;
    let album_i = find(&["album name", "album", "album title"]);
    let dur_ms_i = find(&["duration (ms)", "duration_ms", "track duration (ms)"]);
    let dur_s_i = find(&["duration", "length"]);
    let id_i = find(&["track uri", "spotify - id", "isrc", "id", "track id"]);

    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec?;
        let get = |i: Option<usize>| i.and_then(|i| rec.get(i)).map(str::trim).filter(|s| !s.is_empty());
        let (Some(title), Some(artist)) = (get(Some(title_i)), get(Some(artist_i))) else { continue };
        let duration = get(dur_ms_i)
            .and_then(|d| d.parse::<u64>().ok())
            .map(|ms| (ms / 1000) as u32)
            .or_else(|| get(dur_s_i).and_then(|d| crate::models::parse_duration(d).or_else(|| d.parse().ok())));
        out.push(ImportItem {
            provider_id: get(id_i)
                .map(String::from)
                .unwrap_or_else(|| format!("{}|{}", artist.to_lowercase(), title.to_lowercase())),
            title: title.into(),
            artist: artist.replace(';', ", "),
            album: get(album_i).map(String::from),
            duration,
            video_id: None,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exportify_csv() {
        let csv = "Track URI,Track Name,Artist Name(s),Album Name,Duration (ms)\nspotify:track:1,Get Lucky,\"Daft Punk;Pharrell Williams\",RAM,248000\n";
        let items = parse_text(csv).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].artist, "Daft Punk, Pharrell Williams");
        assert_eq!(items[0].duration, Some(248));
    }

    #[test]
    fn two_way_plan() {
        use super::*;
        let lib = vec![
            (1, "file".to_string(), "Daft Punk".to_string(), "Around the World".to_string()),
            (2, "spotify".to_string(), "Beyonce, Jay-Z".to_string(), "Crazy in Love".to_string()),
            (3, "file".to_string(), "Old Band".to_string(), "Unloved Song".to_string()),
            (4, "file".to_string(), "New Band".to_string(), "Fresh Song".to_string()),
        ];
        let loved = vec![
            ("Daft Punk".to_string(), "Around The World".to_string()),
            ("Beyoncé".to_string(), "Crazy in Love (feat. JAY-Z)".to_string()),
            ("Removed Artist".to_string(), "Removed Song".to_string()),
            ("Lastfm Artist".to_string(), "New Love".to_string()),
        ];
        let mut prev = HashMap::new();
        prev.insert(love_key("Old Band", "Unloved Song"), true);
        prev.insert(love_key("Removed Artist", "Removed Song"), true);
        let p = plan_sync(&lib, &loved, &prev);
        assert_eq!(p.love.iter().map(|x| x.2.as_str()).collect::<Vec<_>>(), vec!["Fresh Song"]);
        assert_eq!(p.add.iter().map(|x| x.2.as_str()).collect::<Vec<_>>(), vec!["New Love"]);
        assert_eq!(p.remove.len(), 1);
        assert_eq!(p.remove[0].1, vec![3]);
        assert_eq!(p.unlove.len(), 1);
        // "Beyonce" (library) and "Beyoncé" (Last.fm) are the same song.
        assert!(p.both.contains(&love_key("Beyonce", "Crazy in Love")));
        assert!(p.both.contains(&love_key("Daft Punk", "Around the World")));
        // First sync ever: nothing is removed.
        let first = plan_sync(&lib, &loved, &HashMap::new());
        assert!(first.remove.is_empty() && first.unlove.is_empty());
        assert_eq!(first.love.len(), 2);
    }

    #[test]
    fn parses_lines() {
        let items = parse_text("Daft Punk - Get Lucky\n\nEminem - Lose Yourself").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].title, "Lose Yourself");
    }
}
