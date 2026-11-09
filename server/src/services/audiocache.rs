//! Short-lived MP3 copies of tracks played from YouTube.
//!
//! When a track streamed from YouTube at the High level has been delivered to a browser in full
//! (every byte, across however many Range requests that took), the bytes the server passed through
//! are transcoded into `<data>/audio-cache/Artist - Title [videoId].mp3`, tagged with the song's
//! title, artist, album and cover art. New plays of that track are then served from the file, as their own source
//! ("cache"), for a configurable time (Settings → Playback; 3 hours by default, or forever, or off).
//! Nothing is downloaded twice: the copy is assembled from the stream the browser was already getting.

use std::{
    collections::HashMap,
    io::SeekFrom,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};

use bytes::Bytes;
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, Semaphore},
};

use crate::db::Db;

/// Setting: how many hours a copy is offered to new plays (0: the cache is off; [`FOREVER`]: copies
/// are kept and used until someone deletes them).
pub const HOURS_KEY: &str = "prefs.audio_cache_hours";
pub const DEFAULT_HOURS: i32 = 3;
pub const MAX_HOURS: i32 = 24 * 30;
pub const FOREVER: i32 = -1;
/// With copies kept forever, how long to remember what a play started from (for its seeks).
const FOREVER_STARTED: Duration = Duration::from_secs(12 * 3600);
/// Files are kept this much longer than they're offered, so a song that started from the cache
/// can still seek when its time runs out mid-song.
const GRACE: Duration = Duration::from_secs(30 * 60);
/// Unfinished copies (the song was skipped before it all loaded) are dropped after this long idle.
const PART_IDLE: Duration = Duration::from_secs(60 * 60);
/// Larger than any song; guards against caching something that isn't one.
const MAX_SOURCE_BYTES: u64 = 200 * 1024 * 1024;
/// MP3 bitrate. YouTube's High level is ~130 kbps AAC/Opus; 192k keeps re-encoding loss inaudible.
pub const KBPS: u32 = 192;
const MAX_COVER_BYTES: usize = 5 * 1024 * 1024;

/// Tags for the MP3. The stream route fills this from yt-dlp's view of the video; the track as
/// YouTube Music lists it (in the database) is preferred when there is one.
#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub thumbnail: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// ffmpeg was found; without it nothing is cached.
    pub available: bool,
    pub hours: i32,
    pub files: u64,
    pub bytes: u64,
}

pub struct AudioCache {
    dir: PathBuf,
    ffmpeg: Option<PathBuf>,
    db: Db,
    http: reqwest::Client,
    hours: AtomicI32,
    /// Finished copies by video id. File names carry the id (`... [videoId].mp3`) but start with
    /// the song's name, so they're found through this index instead of by building the name.
    files: Mutex<HashMap<String, PathBuf>>,
    parts: Mutex<HashMap<String, Arc<Part>>>,
    /// What each track's last from-the-start request was served from (true: the cache). A later
    /// mid-file Range request (a seek) must get bytes of the same file, even if a copy appeared
    /// or expired in between.
    started: Mutex<HashMap<String, (bool, Instant)>>,
    transcode: Semaphore,
}

/// A copy being assembled from proxied byte ranges.
struct Part {
    video_id: String,
    path: PathBuf,
    total: u64,
    meta: Meta,
    state: Mutex<PartState>,
}

struct PartState {
    /// Sorted, non-overlapping [start, end) ranges already written to `path`.
    ranges: Vec<(u64, u64)>,
    done: bool,
    touched: Instant,
}

impl Part {
    /// Record [start, end) as written. True once (for the caller that completed the file).
    fn add(&self, start: u64, end: u64) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.touched = Instant::now();
        if end <= start || s.done {
            return false;
        }
        s.ranges.push((start, end));
        s.ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(s.ranges.len());
        for &(a, b) in &s.ranges {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        s.ranges = merged;
        if s.ranges.first() == Some(&(0, self.total)) {
            s.done = true;
            return true;
        }
        false
    }
}

impl AudioCache {
    pub fn new(data_dir: &Path, db: Db, http: reqwest::Client) -> Self {
        let dir = data_dir.join("audio-cache");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("audio cache: can't create {}: {e}", dir.display());
        }
        let ffmpeg = std::env::var_os("YTP_FFMPEG").map(PathBuf::from).or_else(|| which("ffmpeg"));
        match &ffmpeg {
            Some(p) => tracing::info!("audio cache: using ffmpeg at {}", p.display()),
            None => tracing::warn!("audio cache disabled: ffmpeg not found (install it, or set YTP_FFMPEG)"),
        }
        Self {
            dir,
            ffmpeg,
            db,
            http,
            hours: AtomicI32::new(DEFAULT_HOURS),
            files: Mutex::new(HashMap::new()),
            parts: Mutex::new(HashMap::new()),
            started: Mutex::new(HashMap::new()),
            transcode: Semaphore::new(2),
        }
    }

    /// At startup: read the configured time from the settings and index the copies on disk.
    /// Copies from before files were named after the song (`<videoId>.mp3`) are renamed.
    pub async fn load(&self) {
        let hours = self.db.get_setting(HOURS_KEY).await.ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_HOURS);
        self.hours.store(normalize(hours), Ordering::Relaxed);

        let Ok(mut entries) = tokio::fs::read_dir(&self.dir).await else { return };
        let mut found: HashMap<String, (PathBuf, SystemTime)> = HashMap::new();
        while let Ok(Some(e)) = entries.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            let (id, path) = if let Some(id) = cached_id(&name) {
                (id.to_string(), e.path())
            } else if let Some(id) = legacy_id(&name) {
                let tags = self.tags(id, &Meta::default()).await;
                let to = self.dir.join(format!("{}.mp3", file_stem(id, &tags)));
                match tokio::fs::rename(e.path(), &to).await {
                    Ok(()) => (id.to_string(), to),
                    Err(err) => {
                        tracing::warn!("audio cache: renaming {name}: {err}");
                        (id.to_string(), e.path())
                    }
                }
            } else {
                continue;
            };
            let modified = tokio::fs::metadata(&path).await.and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            // Should there be two copies of one song, use the newer.
            if found.get(&id).is_none_or(|(_, m)| modified > *m) {
                found.insert(id, (path, modified));
            }
        }
        let mut files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        files.extend(found.into_iter().map(|(id, (path, _))| (id, path)));
    }

    /// The ffmpeg this server found, if any.
    pub fn ffmpeg(&self) -> Option<&Path> {
        self.ffmpeg.as_deref()
    }

    pub fn hours(&self) -> i32 {
        self.hours.load(Ordering::Relaxed)
    }

    /// Change how long copies are offered (0: off, [`FOREVER`]: never expire). A shorter time
    /// applies to existing copies too.
    pub async fn set_hours(self: &Arc<Self>, hours: i32) -> anyhow::Result<()> {
        let hours = normalize(hours);
        self.db.set_setting(HOURS_KEY, &hours.to_string()).await?;
        self.hours.store(hours, Ordering::Relaxed);
        let this = self.clone();
        tokio::spawn(async move { this.purge().await });
        Ok(())
    }

    fn enabled(&self) -> bool {
        self.hours() != 0
    }

    /// How long copies are offered; None when they're kept forever.
    fn ttl(&self) -> Option<Duration> {
        match self.hours() {
            FOREVER => None,
            h => Some(Duration::from_secs(h.max(0) as u64 * 3600)),
        }
    }

    pub async fn stats(&self) -> Stats {
        let (mut files, mut bytes) = (0, 0);
        if let Ok(mut entries) = tokio::fs::read_dir(&self.dir).await {
            while let Ok(Some(e)) = entries.next_entry().await {
                if e.file_name().to_string_lossy().ends_with(".mp3") {
                    files += 1;
                    bytes += e.metadata().await.map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        Stats { available: self.ffmpeg.is_some(), hours: self.hours(), files, bytes }
    }

    /// The finished copy of a track, if there is one.
    fn file(&self, video_id: &str) -> Option<PathBuf> {
        self.files.lock().unwrap_or_else(|p| p.into_inner()).get(video_id).cloned()
    }

    /// Age of the finished copy, if there is one.
    fn age(&self, video_id: &str) -> Option<Duration> {
        let modified = std::fs::metadata(self.file(video_id)?).ok()?.modified().ok()?;
        Some(SystemTime::now().duration_since(modified).unwrap_or_default())
    }

    fn fresh(&self, video_id: &str) -> bool {
        self.enabled() && self.age(video_id).is_some_and(|a| self.ttl().is_none_or(|ttl| a < ttl))
    }

    /// Songs with a copy that new plays would use (for searching while YouTube is unavailable).
    pub fn fresh_ids(&self) -> Vec<String> {
        let ids: Vec<String> = self.files.lock().unwrap_or_else(|p| p.into_inner()).keys().cloned().collect();
        ids.into_iter().filter(|id| self.fresh(id)).collect()
    }

    /// Whether `/source` should report the cache: a fresh copy, and the client's quality allows it.
    pub fn peek(&self, video_id: &str, allowed: bool) -> bool {
        allowed && self.fresh(video_id)
    }

    /// The copy to serve this `/stream` request from, if any. `from_start`: the request reads from
    /// byte 0 (a new load) rather than continuing a load mid-file (a seek). `allowed`: the client's
    /// quality budget permits the cache's bitrate.
    pub fn choose(&self, video_id: &str, from_start: bool, allowed: bool) -> Option<PathBuf> {
        let mut started = self.started.lock().unwrap_or_else(|p| p.into_inner());
        let use_it = if from_start {
            let use_it = self.peek(video_id, allowed);
            started.insert(video_id.to_string(), (use_it, Instant::now()));
            use_it
        } else {
            match started.get(video_id) {
                // Keep reading the file the song started from (it's kept through the grace period).
                Some((true, _)) => true,
                Some((false, _)) => false,
                None => self.peek(video_id, allowed),
            }
        };
        if use_it { self.file(video_id) } else { None }
    }

    /// Start copying a proxied YouTube response that carries bytes `offset..` of a `total`-byte
    /// file. Send each body chunk to the returned channel, in order. None when there's nothing to
    /// do (cache off, no ffmpeg, already cached, or already complete).
    pub fn tee(self: &Arc<Self>, video_id: &str, offset: u64, total: u64, meta: Meta) -> Option<mpsc::UnboundedSender<Bytes>> {
        let ffmpeg = self.ffmpeg.clone()?;
        if !self.enabled() || total == 0 || total > MAX_SOURCE_BYTES || offset >= total || self.fresh(video_id) {
            return None;
        }
        let part = {
            let mut parts = self.parts.lock().unwrap_or_else(|p| p.into_inner());
            match parts.get(video_id) {
                Some(p) if p.total == total => p.clone(),
                // New, or a different file (e.g. a re-resolved format): start over. Writers of
                // the old one keep their own file, which the purge loop removes.
                _ => {
                    let p = Arc::new(Part {
                        video_id: video_id.to_string(),
                        path: self.dir.join(format!("{}.{total}.part", file_stem(video_id, &meta))),
                        total,
                        meta,
                        state: Mutex::new(PartState { ranges: Vec::new(), done: false, touched: Instant::now() }),
                    });
                    parts.insert(video_id.to_string(), p.clone());
                    p
                }
            }
        };
        if part.state.lock().unwrap_or_else(|p| p.into_inner()).done {
            return None;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let this = self.clone();
        tokio::spawn(async move {
            match this.write(&part, offset, rx).await {
                Ok(true) => this.finish(&part, &ffmpeg).await,
                Ok(false) => {}
                Err(e) => tracing::warn!("audio cache: writing {}: {e:#}", part.video_id),
            }
        });
        Some(tx)
    }

    /// Write chunks at their offsets. Ok(true) when this writer completed the file.
    async fn write(&self, part: &Part, mut offset: u64, mut rx: mpsc::UnboundedReceiver<Bytes>) -> anyhow::Result<bool> {
        let mut f = tokio::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&part.path).await?;
        f.seek(SeekFrom::Start(offset)).await?;
        while let Some(chunk) = rx.recv().await {
            let end = (offset + chunk.len() as u64).min(part.total);
            if end <= offset {
                break;
            }
            f.write_all(&chunk[..(end - offset) as usize]).await?;
            // Only count bytes that are really in the file.
            f.flush().await?;
            let complete = part.add(offset, end);
            offset = end;
            if complete {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn finish(&self, part: &Arc<Part>, ffmpeg: &Path) {
        let vid = &part.video_id;
        let _permit = self.transcode.acquire().await;
        let tags = self.tags(vid, &part.meta).await;
        let stem = file_stem(vid, &tags);
        let out = self.dir.join(format!("{stem}.mp3"));
        let tmp = self.dir.join(format!("{stem}.mp3.tmp"));
        let cover = match &tags.thumbnail {
            Some(url) => self.fetch_cover(vid, url).await,
            None => None,
        };

        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&part.path);
        if let Some((path, _)) = &cover {
            cmd.arg("-i").arg(path);
        }
        cmd.args(["-map", "0:a", "-c:a", "libmp3lame", "-b:a", &format!("{KBPS}k")]);
        if let Some((_, codec)) = &cover {
            cmd.args(["-map", "1:v", "-c:v", codec, "-disposition:v:0", "attached_pic"])
                .args(["-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"]);
        }
        // Drop the source's own tags ("ISO Media file produced by Google…") and write the song's.
        cmd.args(["-map_metadata", "-1"]);
        for (key, value) in tags.pairs(vid) {
            cmd.arg("-metadata").arg(format!("{key}={value}"));
        }
        // ID3v2.3 is what most players and file managers read best.
        cmd.args(["-id3v2_version", "3", "-write_id3v1", "1", "-f", "mp3"]).arg(&tmp);
        let result = tokio::time::timeout(
            Duration::from_secs(10 * 60),
            cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).output(),
        )
        .await;
        if let Some((path, _)) = &cover {
            let _ = tokio::fs::remove_file(path).await;
        }
        match result {
            Ok(Ok(o)) if o.status.success() => match tokio::fs::rename(&tmp, &out).await {
                Ok(()) => {
                    // An older copy under another name (the song was renamed) is left for the
                    // purge loop, in case a play that started from it is still seeking.
                    self.files.lock().unwrap_or_else(|p| p.into_inner()).insert(vid.clone(), out);
                    match self.ttl() {
                        Some(ttl) => tracing::info!("audio cache: saved \"{stem}.mp3\" for {} h", ttl.as_secs() / 3600),
                        None => tracing::info!("audio cache: saved \"{stem}.mp3\" (kept forever)"),
                    }
                }
                Err(e) => tracing::warn!("audio cache: saving {vid}: {e}"),
            },
            Ok(Ok(o)) => {
                let err = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("audio cache: ffmpeg failed for {vid}: {}", err.trim().chars().take(300).collect::<String>());
            }
            Ok(Err(e)) => tracing::warn!("audio cache: running ffmpeg: {e}"),
            Err(_) => tracing::warn!("audio cache: ffmpeg timed out for {vid}"),
        }
        let _ = tokio::fs::remove_file(&tmp).await;
        let _ = tokio::fs::remove_file(&part.path).await;
        let mut parts = self.parts.lock().unwrap_or_else(|p| p.into_inner());
        if parts.get(vid).is_some_and(|p| Arc::ptr_eq(p, part)) {
            parts.remove(vid);
        }
    }

    /// The song's tags: YouTube Music's listing when the track is known, else what yt-dlp said.
    async fn tags(&self, video_id: &str, meta: &Meta) -> Meta {
        let Some(t) = self.db.get_track(video_id).await.ok().flatten() else { return meta.clone() };
        let pick = |a: Option<String>, b: &Option<String>| a.filter(|s| !s.trim().is_empty()).or_else(|| b.clone());
        Meta {
            title: pick(Some(t.title), &meta.title),
            artist: pick(Some(t.artist), &meta.artist),
            album: pick(t.album, &meta.album),
            thumbnail: pick(t.thumbnail, &meta.thumbnail),
        }
    }

    /// Download the cover art next to the copy. Returns the file and the ffmpeg codec to embed it
    /// with: JPEG and PNG as they are, WebP converted to JPEG (MP3 players don't show WebP).
    async fn fetch_cover(&self, video_id: &str, url: &str) -> Option<(PathBuf, &'static str)> {
        if !url.starts_with("https://") {
            return None;
        }
        let resp = self.http.get(url).timeout(Duration::from_secs(20)).send().await.ok()?.error_for_status().ok()?;
        let bytes = resp.bytes().await.ok()?;
        if bytes.len() > MAX_COVER_BYTES {
            return None;
        }
        let codec = match &bytes[..] {
            [0xFF, 0xD8, ..] | [0x89, b'P', b'N', b'G', ..] => "copy",
            [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "mjpeg",
            _ => return None,
        };
        let path = self.dir.join(format!("{video_id}.cover.tmp"));
        tokio::fs::write(&path, &bytes).await.ok()?;
        Some((path, codec))
    }

    /// Delete expired copies and abandoned partial files.
    async fn purge(&self) {
        let now = SystemTime::now();
        // How long finished copies stay on disk; None: forever.
        let keep_mp3 = self.ttl().map(|ttl| ttl + GRACE);
        {
            let mut parts = self.parts.lock().unwrap_or_else(|p| p.into_inner());
            parts.retain(|_, p| p.state.lock().unwrap_or_else(|e| e.into_inner()).touched.elapsed() < PART_IDLE);
            let mut started = self.started.lock().unwrap_or_else(|p| p.into_inner());
            let keep_started = keep_mp3.unwrap_or(FOREVER_STARTED);
            started.retain(|_, (_, at)| at.elapsed() < keep_started);
        }
        let Ok(mut entries) = tokio::fs::read_dir(&self.dir).await else { return };
        let mut removed = 0;
        while let Ok(Some(e)) = entries.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            let keep_for = if name.ends_with(".mp3") {
                let Some(keep) = keep_mp3 else { continue };
                keep
            } else if name.ends_with(".part") || name.ends_with(".tmp") {
                PART_IDLE
            } else {
                continue;
            };
            let Ok(modified) = e.metadata().await.and_then(|m| m.modified()) else { continue };
            let path = e.path();
            if now.duration_since(modified).unwrap_or_default() > keep_for && tokio::fs::remove_file(&path).await.is_ok() {
                removed += 1;
                if let Some(id) = cached_id(&name) {
                    let mut files = self.files.lock().unwrap_or_else(|p| p.into_inner());
                    if files.get(id) == Some(&path) {
                        files.remove(id);
                    }
                }
            }
        }
        if removed > 0 {
            tracing::debug!("audio cache: removed {removed} expired files");
        }
    }

    pub fn spawn_purge_loop(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(10 * 60));
            loop {
                t.tick().await;
                this.purge().await;
            }
        });
    }
}

impl Meta {
    /// ffmpeg `-metadata` pairs. The comment links back to the song on YouTube Music.
    fn pairs(&self, video_id: &str) -> Vec<(&'static str, String)> {
        let clean = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(String::from);
        let mut out = Vec::new();
        for (key, value) in [("title", &self.title), ("artist", &self.artist), ("album", &self.album)] {
            if let Some(v) = clean(value) {
                out.push((key, v));
            }
        }
        if let Some(a) = clean(&self.artist) {
            out.push(("album_artist", a));
        }
        out.push(("comment", format!("https://music.youtube.com/watch?v={video_id}")));
        out
    }
}

/// A valid hours setting: [`FOREVER`], or 0 (off) to [`MAX_HOURS`].
fn normalize(hours: i32) -> i32 {
    if hours == FOREVER { FOREVER } else { hours.clamp(0, MAX_HOURS) }
}

/// Longest the "Artist - Title" part of a name may be, in bytes (file systems allow 255).
const MAX_LABEL_BYTES: usize = 150;

/// "Artist - Title [videoId]": readable, and safe as a file name on Windows, macOS, Linux and
/// network shares. The id at the end keeps names unique and lets [`cached_id`] read it back.
fn file_stem(video_id: &str, meta: &Meta) -> String {
    let clean = |v: &Option<String>| v.as_deref().map(safe_name).filter(|s| !s.is_empty());
    let label = match (clean(&meta.artist), clean(&meta.title)) {
        (Some(a), Some(t)) => format!("{a} - {t}"),
        (a, t) => a.or(t).unwrap_or_default(),
    };
    let mut end = label.len().min(MAX_LABEL_BYTES);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    let label = label[..end].trim_end_matches(['.', ' ']);
    if label.is_empty() { format!("[{video_id}]") } else { format!("{label} [{video_id}]") }
}

/// Drop or replace characters that aren't allowed in file names somewhere.
fn safe_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '/' | '\\' | '|' => out.push('-'),
            ':' => out.push_str(" -"),
            '"' => out.push('\''),
            '<' | '>' | '?' | '*' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    out.trim_matches(['.', ' ']).to_string()
}

/// The video id of a finished copy, from its name (`... [videoId].mp3`).
fn cached_id(name: &str) -> Option<&str> {
    let (_, id) = name.strip_suffix("].mp3")?.rsplit_once('[')?;
    is_video_id(id).then_some(id)
}

/// The video id of a copy named the old way (`<videoId>.mp3`).
fn legacy_id(name: &str) -> Option<&str> {
    name.strip_suffix(".mp3").filter(|id| is_video_id(id))
}

fn is_video_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::split_paths(&path).map(|d| d.join(&exe)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(total: u64) -> Part {
        Part {
            video_id: "x".into(),
            path: PathBuf::new(),
            total,
            meta: Meta::default(),
            state: Mutex::new(PartState { ranges: Vec::new(), done: false, touched: Instant::now() }),
        }
    }

    #[test]
    fn completes_from_out_of_order_overlapping_ranges() {
        let p = part(100);
        assert!(!p.add(0, 2)); // Safari's probe
        assert!(!p.add(50, 100)); // a seek
        assert!(!p.add(0, 30));
        assert!(p.add(20, 60));
        assert!(!p.add(0, 100), "only reported complete once");
    }

    #[test]
    fn single_full_response_completes() {
        let p = part(10);
        assert!(p.add(0, 10));
    }

    #[test]
    fn gap_is_not_complete() {
        let p = part(10);
        assert!(!p.add(0, 4));
        assert!(!p.add(5, 10));
    }

    #[test]
    fn tag_pairs_skip_blanks_and_link_back() {
        let m = Meta { title: Some("Song".into()), artist: Some("Band".into()), album: Some("  ".into()), thumbnail: None };
        let pairs = m.pairs("abc");
        let keys: Vec<_> = pairs.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["title", "artist", "album_artist", "comment"]);
        assert_eq!(pairs.last().unwrap().1, "https://music.youtube.com/watch?v=abc");
    }

    #[test]
    fn hours_settings_normalize() {
        assert_eq!(normalize(FOREVER), FOREVER);
        assert_eq!(normalize(0), 0);
        assert_eq!(normalize(-5), 0);
        assert_eq!(normalize(6), 6);
        assert_eq!(normalize(100_000), MAX_HOURS);
    }

    fn meta(artist: Option<&str>, title: Option<&str>) -> Meta {
        Meta { artist: artist.map(Into::into), title: title.map(Into::into), ..Meta::default() }
    }

    #[test]
    fn file_names_are_readable_and_safe() {
        assert_eq!(file_stem("abc", &meta(Some("Band"), Some("Song"))), "Band - Song [abc]");
        assert_eq!(file_stem("abc", &meta(None, Some("Song"))), "Song [abc]");
        assert_eq!(file_stem("abc", &meta(Some("  "), None)), "[abc]");
        assert_eq!(
            file_stem("abc", &meta(Some("AC/DC"), Some("What? \"Live\": Part 1 <Remix>..."))),
            "AC-DC - What 'Live' - Part 1 Remix [abc]"
        );
        let long = "ü".repeat(200);
        let stem = file_stem("abc", &meta(None, Some(&long)));
        assert!(stem.len() <= MAX_LABEL_BYTES + " [abc]".len() && stem.ends_with(" [abc]"));
    }

    #[test]
    fn ids_read_back_from_names() {
        let name = format!("{}.mp3", file_stem("8AzwKXt0a3c", &meta(Some("Band"), Some("Song [Live]"))));
        assert_eq!(cached_id(&name), Some("8AzwKXt0a3c"));
        assert_eq!(cached_id("[a-_9].mp3"), Some("a-_9"));
        assert_eq!(cached_id("Band - Song [bad id].mp3"), None);
        assert_eq!(cached_id("Band - Song [abc].mp3.tmp"), None);
        assert_eq!(legacy_id("8AzwKXt0a3c.mp3"), Some("8AzwKXt0a3c"));
        assert_eq!(legacy_id("Band - Song [abc].mp3"), None);
    }
}
