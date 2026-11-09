//! yt-dlp integration.
//!
//! The binary is located in this order:
//!   1. `YTDLP_PATH`
//!   2. compiled into the server (`--features embed-ytdlp`), extracted to the data dir
//!   3. a previously downloaded copy in `<data>/bin/yt-dlp`
//!   4. `yt-dlp` on `$PATH`
//!   5. downloaded automatically from the official GitHub release
//!
//! Audio is served by resolving the direct googlevideo URL and proxying it with
//! Range support (so seeking works), falling back to piping yt-dlp's stdout.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::Value;
use tokio::{process::Command, sync::Mutex};

const FORMAT: &str = "bestaudio[ext=m4a]/bestaudio[acodec=opus]/bestaudio/best";
/// AAC-LC for browsers without Opus, at every level (see [`AudioLevel::format`]).
const FORMAT_NO_HEAAC: &str = "140/bestaudio[ext=m4a][format_id!=139]/bestaudio[ext=m4a]/bestaudio/best";
const URL_TTL: Duration = Duration::from_secs(60 * 60 * 4);
/// YouTube clients yt-dlp asks first (`YTP_YOUTUBE_CLIENTS` overrides; empty: yt-dlp's choice).
/// With a JS runtime (the Docker image has Deno) yt-dlp's own choice is the TV client, whose
/// audio URLs googlevideo mostly answers with 403 now; the visionOS client's play.
const DEFAULT_CLIENTS: &str = "visionos";

#[cfg(feature = "embed-ytdlp")]
static EMBEDDED: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/vendor/yt-dlp"));

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedStream {
    pub url: String,
    pub mime: String,
    pub content_length: Option<u64>,
    #[serde(skip)]
    pub headers: HashMap<String, String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<u32>,
    pub thumbnail: Option<String>,
    /// Audio bitrate in kbps, when yt-dlp reports it.
    pub kbps: Option<u32>,
    /// A livestream: `url` is an HLS playlist (YouTube offers live audio only that way), not a file.
    pub live: bool,
}

/// How much audio data to send. The browser picks it from how fast its connection to this
/// server is (a bitrate budget); the server maps that to a YouTube format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioLevel {
    /// ~130 kbps (AAC 140 / Opus 251)
    High,
    /// ~66 kbps (Opus 250); AAC 140 where Opus can't play
    Medium,
    /// ~50 kbps (Opus 249); AAC 140 where Opus can't play
    Low,
}

impl AudioLevel {
    /// The richest level that fits `budget_kbps` (None: no limit).
    pub fn for_budget(budget_kbps: Option<u32>) -> Self {
        match budget_kbps {
            None => Self::High,
            Some(k) if k >= 128 => Self::High,
            Some(k) if k >= 64 => Self::Medium,
            Some(_) => Self::Low,
        }
    }

    /// yt-dlp format selector. `opus`: the browser can play WebM/Opus (Safari often can't).
    ///
    /// Never HE-AAC (139): Safari (iOS especially) misreads its length as twice the real one, so
    /// the song shows double its time and the second half is silence. Without Opus there's no
    /// smaller format that plays right, so every level is AAC-LC 140 there.
    fn format(self, opus: bool) -> &'static str {
        match (self, opus) {
            (Self::High, _) => FORMAT,
            (Self::Medium, true) => "250/249/bestaudio[acodec=opus]/bestaudio[ext=m4a][format_id!=139]/bestaudio/best",
            (Self::Low, true) => "249/250/bestaudio[acodec=opus]/bestaudio[ext=m4a][format_id!=139]/bestaudio/best",
            (_, false) => FORMAT_NO_HEAAC,
        }
    }

    pub fn nominal_kbps(self, opus: bool) -> u32 {
        match (self, opus) {
            (Self::High, _) | (_, false) => 130,
            (Self::Medium, true) => 66,
            (Self::Low, true) => 50,
        }
    }
}

pub struct YtDlp {
    http: reqwest::Client,
    configured: Option<PathBuf>,
    data_dir: PathBuf,
    /// Where the managed copy lives (`<data>/bin` unless `YTP_BIN_DIR` says otherwise).
    bin_dir: PathBuf,
    bin: Mutex<Option<PathBuf>>,
    /// `--version` output. Starting yt-dlp takes seconds (it unpacks itself), so ask once.
    version: Mutex<Option<String>>,
    cache: Mutex<HashMap<String, (ResolvedStream, Instant)>>,
    /// `player_client` for yt-dlp, tried before its defaults. None: only its defaults.
    clients: Option<String>,
}

impl YtDlp {
    pub fn new(http: reqwest::Client, configured: Option<PathBuf>, data_dir: PathBuf, bin_dir: PathBuf) -> Self {
        Self {
            http,
            configured,
            data_dir,
            bin_dir,
            bin: Mutex::new(None),
            version: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            clients: match std::env::var("YTP_YOUTUBE_CLIENTS") {
                Ok(v) => Some(v.trim().to_string()).filter(|v| !v.is_empty()),
                Err(_) => Some(DEFAULT_CLIENTS.to_string()),
            },
        }
    }

    fn managed_path(&self) -> PathBuf {
        let name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
        self.bin_dir.join(name)
    }

    /// Returns a usable yt-dlp path, installing it if necessary.
    pub async fn binary(&self) -> anyhow::Result<PathBuf> {
        let mut guard = self.bin.lock().await;
        if let Some(p) = guard.as_ref() {
            return Ok(p.clone());
        }
        let path = self.locate().await?;
        tracing::info!("using yt-dlp at {}", path.display());
        *guard = Some(path.clone());
        Ok(path)
    }

    async fn locate(&self) -> anyhow::Result<PathBuf> {
        if let Some(p) = &self.configured {
            return Ok(p.clone());
        }
        let managed = self.managed_path();

        #[cfg(feature = "embed-ytdlp")]
        if !managed.exists() {
            write_executable(&managed, EMBEDDED).await?;
            return Ok(managed);
        }

        if managed.exists() {
            return Ok(managed);
        }
        if let Some(p) = which("yt-dlp") {
            return Ok(p);
        }
        self.download(&managed).await?;
        Ok(managed)
    }

    async fn download(&self, dest: &Path) -> anyhow::Result<()> {
        let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => "yt-dlp_linux",
            ("linux", "aarch64") => "yt-dlp_linux_aarch64",
            ("linux", "arm") => "yt-dlp_linux_armv7l",
            ("macos", _) => "yt-dlp_macos",
            ("windows", _) => "yt-dlp.exe",
            _ => "yt-dlp", // python zipapp, needs python3
        };
        let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}");
        tracing::info!("downloading yt-dlp from {url}");
        let resp = self.http.get(&url).send().await?.error_for_status()?;
        let bytes = resp.bytes().await?;
        write_executable(dest, &bytes).await
    }

    pub async fn version(&self) -> anyhow::Result<String> {
        let mut cached = self.version.lock().await;
        if let Some(v) = cached.as_ref() {
            return Ok(v.clone());
        }
        let bin = self.binary().await?;
        let out = Command::new(bin).arg("--version").output().await?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !v.is_empty() {
            *cached = Some(v.clone());
        }
        Ok(v)
    }

    /// Self-update. Only works for the managed binary; a system package should be updated by its package manager.
    pub async fn update(&self) -> anyhow::Result<String> {
        let bin = self.binary().await?;
        *self.version.lock().await = None;
        if bin == self.managed_path() {
            let out = Command::new(&bin).arg("-U").output().await?;
            let mut s = String::from_utf8_lossy(&out.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            if !out.status.success() {
                // The zipapp/embedded copy may not be able to self-update: re-download instead.
                self.download(&bin).await?;
                s.push_str("\nre-downloaded latest release");
            }
            Ok(s.trim().to_string())
        } else {
            bail!("yt-dlp at {} is not managed by ytp; update it with your package manager", bin.display())
        }
    }

    pub async fn invalidate(&self, video_id: &str) {
        let prefix = format!("{video_id}|");
        self.cache.lock().await.retain(|k, _| !k.starts_with(&prefix));
    }

    /// Resolve the direct audio URL (cached for a few hours; googlevideo URLs expire after ~6h).
    pub async fn resolve(&self, video_id: &str, level: AudioLevel, opus: bool) -> anyhow::Result<ResolvedStream> {
        let format = level.format(opus);
        let key = format!("{video_id}|{format}");
        // The stream and the quality lookup ask at the same moment: run yt-dlp once.
        let (_flight, _) = crate::services::cache::Flight::join(&format!("ytdlp:{key}")).await;
        if let Some((s, at)) = self.cache.lock().await.get(&key) {
            if at.elapsed() < URL_TTL {
                return Ok(s.clone());
            }
        }
        let bin = self.binary().await?;
        let detail = if level == AudioLevel::High { video_id.to_string() } else { format!("{video_id} ({level:?})") };
        let timer = crate::services::ytlog::start("ytdlp", "resolve", detail);
        let run = |clients: Option<&str>| {
            let mut cmd = Command::new(&bin);
            cmd.args(["-f", format, "-j", "--no-playlist", "--no-warnings", "--no-progress"]);
            if let Some(c) = clients {
                cmd.arg("--extractor-args").arg(format!("youtube:player_client={c}"));
            }
            cmd.arg(watch_url(video_id)).stdin(Stdio::null()).output()
        };
        let mut out = run(self.clients.as_deref()).await;
        // The preferred clients can't get this video (or yt-dlp no longer knows them): yt-dlp's own.
        if self.clients.is_some() && out.as_ref().is_ok_and(|o| !o.status.success()) {
            let err = out.as_ref().map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string()).unwrap_or_default();
            tracing::info!("yt-dlp with player_client {} failed for {video_id}, trying its defaults: {}", self.clients.as_deref().unwrap_or(""), err.chars().take(200).collect::<String>());
            out = run(None).await;
        }
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                timer.done("error", None, 1, Some(e.to_string()));
                return Err(anyhow::Error::new(e).context("running yt-dlp"));
            }
        };
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let outcome = if err.contains("429") || err.contains("Sign in to confirm") { "rate-limited" } else { "error" };
            timer.done(outcome, None, 1, Some(err.chars().take(200).collect()));
            bail!("yt-dlp failed: {err}");
        }
        timer.done("ok", None, 1, None);
        let v: Value = serde_json::from_slice(&out.stdout).context("parsing yt-dlp json")?;
        let url = v["url"].as_str().context("yt-dlp returned no url")?.to_string();
        let ext = v["ext"].as_str().unwrap_or("m4a");
        let mime = match ext {
            "m4a" | "mp4" => "audio/mp4",
            "webm" => "audio/webm",
            "mp3" => "audio/mpeg",
            "ogg" | "opus" => "audio/ogg",
            _ => "application/octet-stream",
        }
        .to_string();
        let headers = v["http_headers"]
            .as_object()
            .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
            .unwrap_or_default();
        let s = |k: &str| v[k].as_str().map(String::from);
        let resolved = ResolvedStream {
            url,
            mime,
            content_length: v["filesize"].as_u64().or_else(|| v["filesize_approx"].as_u64()),
            headers,
            title: s("track").or_else(|| s("title")),
            artist: s("artist").or_else(|| s("uploader").map(|u| u.trim_end_matches(" - Topic").to_string())),
            album: s("album"),
            duration: v["duration"].as_f64().map(|d| d.round() as u32),
            thumbnail: s("thumbnail"),
            kbps: v["abr"].as_f64().or_else(|| v["tbr"].as_f64()).map(|k| k.round() as u32),
            live: v["is_live"].as_bool() == Some(true) || v["protocol"].as_str().is_some_and(|p| p.starts_with("m3u8")),
        };
        self.cache.lock().await.insert(key, (resolved.clone(), Instant::now()));
        Ok(resolved)
    }

    /// Spawn yt-dlp writing the audio to stdout. The child is killed when the returned value is dropped.
    pub async fn pipe(&self, video_id: &str, level: AudioLevel, opus: bool) -> anyhow::Result<tokio::process::Child> {
        let bin = self.binary().await?;
        crate::services::ytlog::start("ytdlp", "pipe", video_id).done("ok", None, 1, None);
        let mut cmd = Command::new(bin);
        cmd.args(["-f", level.format(opus), "-o", "-", "--quiet", "--no-playlist", "--no-part"]);
        if let Some(c) = &self.clients {
            cmd.arg("--extractor-args").arg(format!("youtube:player_client={c}"));
        }
        let child = cmd
            .arg(watch_url(video_id))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning yt-dlp")?;
        Ok(child)
    }
}

/// A cookie read from a local browser profile.
#[derive(Debug, Clone)]
pub struct BrowserCookie {
    pub domain: String,
    pub name: String,
    pub value: String,
}

impl YtDlp {
    /// Read the cookies of a browser installed on this machine (`firefox`, `chrome`, `chromium`,
    /// `brave`, `edge`, `vivaldi`, `opera`, optionally `name:profile`) using yt-dlp's
    /// `--cookies-from-browser`, which handles profile discovery (incl. snap/flatpak) and
    /// Chromium cookie decryption. Works offline: yt-dlp dumps the jar and exits.
    pub async fn browser_cookies(&self, browser: &str) -> anyhow::Result<Vec<BrowserCookie>> {
        let bin = self.binary().await?;
        // Private, short-lived directory: these are session secrets.
        let dir = self.data_dir.join(format!("cookies-{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&dir).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
        }
        let file = dir.join("cookies.txt");
        let result = async {
            let out = tokio::time::timeout(
                Duration::from_secs(60),
                Command::new(bin)
                    .args(["--ignore-config", "--no-warnings", "--cookies-from-browser", browser, "--cookies"])
                    .arg(&file)
                    .stdin(Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .context("timed out reading browser cookies")??;
            // Without a URL yt-dlp exits with status 2 *after* writing the jar; only the file matters.
            let text = tokio::fs::read_to_string(&file).await.map_err(|_| {
                let err = String::from_utf8_lossy(&out.stderr);
                let line = err.lines().find(|l| l.contains("ERROR")).unwrap_or("no cookie store found").trim().to_string();
                anyhow::anyhow!("{browser}: {line}")
            })?;
            Ok(parse_netscape_cookies(&text))
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result
    }
}

/// Parse a Netscape/Mozilla cookies.txt file.
pub fn parse_netscape_cookies(text: &str) -> Vec<BrowserCookie> {
    text.lines()
        .filter_map(|line| {
            // "#HttpOnly_" marks HttpOnly cookies; other '#' lines are comments.
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.starts_with('#') || line.trim().is_empty() {
                return None;
            }
            let f: Vec<&str> = line.split('\t').collect();
            (f.len() >= 7).then(|| BrowserCookie { domain: f[0].to_string(), name: f[5].to_string(), value: f[6].to_string() })
        })
        .collect()
}

fn watch_url(video_id: &str) -> String {
    format!("https://music.youtube.com/watch?v={video_id}")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

async fn write_executable(dest: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = dest.with_extension("tmp");
    tokio::fs::write(&tmp, bytes).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).await?;
    }
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}
