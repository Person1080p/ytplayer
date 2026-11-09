use std::{net::SocketAddr, path::PathBuf};

#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "embedded-tailscale"), allow(dead_code))]
pub struct TailscaleConfig {
    /// Run an embedded tailscale node (requires the `embedded-tailscale` feature; default on in such builds).
    pub embedded: bool,
    pub hostname: String,
    pub authkey: Option<String>,
    pub state_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub web_dir: PathBuf,
    /// Externally reachable base URL (e.g. https://music.tailnet-xyz.ts.net). Used for OAuth callbacks.
    pub public_url: Option<String>,
    /// Optional shared secret required to use the API.
    pub access_token: Option<String>,
    pub lastfm_api_key: Option<String>,
    pub lastfm_api_secret: Option<String>,
    pub ytdlp_path: Option<PathBuf>,
    /// Where the managed yt-dlp is kept. Defaults to `<data>/bin`; point it at a local disk when
    /// the data dir is on a network share, since yt-dlp is read in full on every start.
    pub bin_dir: PathBuf,
    /// A browser for the server that's used through a web page (e.g. a Firefox container's web UI).
    /// Settings → YouTube Music then sends the user there to sign in, instead of to Google.
    pub signin_browser_url: Option<String>,
    pub tailscale: TailscaleConfig,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let data_dir = PathBuf::from(env("YTP_DATA_DIR").unwrap_or_else(|| "./data".into()));
        std::fs::create_dir_all(&data_dir)?;
        let bind = env("YTP_BIND").unwrap_or_else(|| "127.0.0.1:8080".into()).parse()?;
        Ok(Self {
            bind,
            web_dir: PathBuf::from(env("YTP_WEB_DIR").unwrap_or_else(|| "../web/dist".into())),
            public_url: env("YTP_PUBLIC_URL").map(|s| s.trim_end_matches('/').to_string()),
            access_token: env("YTP_ACCESS_TOKEN"),
            lastfm_api_key: env("LASTFM_API_KEY"),
            lastfm_api_secret: env("LASTFM_API_SECRET"),
            ytdlp_path: env("YTDLP_PATH").map(PathBuf::from),
            bin_dir: env("YTP_BIN_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir.join("bin")),
            signin_browser_url: env("YTP_SIGNIN_BROWSER_URL"),
            tailscale: TailscaleConfig {
                // Builds with the embedded node use it unless told otherwise (TS_EMBEDDED=0).
                embedded: env("TS_EMBEDDED")
                    .map(|v| v == "1" || v == "true")
                    .unwrap_or(cfg!(feature = "embedded-tailscale")),
                hostname: env("TS_HOSTNAME").unwrap_or_else(|| "ytp".into()),
                authkey: env("TS_AUTHKEY"),
                state_dir: env("TS_STATE_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir.join("tailscale")),
            },
            data_dir,
        })
    }
}
