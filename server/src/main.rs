mod config;
mod db;
mod error;
mod models;
mod routes;
mod services;
mod state;

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::{broadcast, Mutex};
use tracing_subscriber::EnvFilter;

use crate::{
    config::Config,
    db::Db,
    services::{lastfm::LastFm, playback, plex::Plex, queue::Queue, tailscale, ytdlp::YtDlp, ytmusic::YtMusic},
    state::AppState,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "ytp_server=info,tower_http=warn".into()))
        .init();

    let cfg = Config::from_env()?;
    let db = Db::open(&cfg.data_dir.join("ytp.sqlite3"))?;
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(60))
        .build()?;

    services::ytlog::init(&cfg.data_dir, db.get_bool(services::ytlog::PREF, false).await);

    let (events, _) = broadcast::channel(256);
    let state = Arc::new(AppState {
        yt: YtMusic::new(http.clone(), db.clone()),
        ytdlp: YtDlp::new(http.clone(), cfg.ytdlp_path.clone(), cfg.data_dir.clone(), cfg.bin_dir.clone()),
        audio_cache: Arc::new(services::audiocache::AudioCache::new(&cfg.data_dir, db.clone(), http.clone())),
        plex: Plex::new(http.clone(), db.clone()),
        lastfm: LastFm::new(http.clone(), db.clone(), cfg.lastfm_api_key.clone(), cfg.lastfm_api_secret.clone()),
        queue: Mutex::new(Queue::load(&db).await),
        sessions: Mutex::new(HashMap::new()),
        tailscale: Arc::new(Mutex::new(tailscale::TailscaleInfo::default())),
        events,
        http,
        db,
        cfg: cfg.clone(),
    });

    state.yt.set_pace(&routes::settings::import_pace(&state).await);

    // Make sure yt-dlp is available before the first play request (downloads it if needed).
    {
        let st = state.clone();
        tokio::spawn(async move {
            match st.ytdlp.version().await {
                Ok(v) => tracing::info!("yt-dlp {v} ready"),
                Err(e) => tracing::error!("yt-dlp unavailable: {e:#}"),
            }
        });
    }
    playback::spawn_retry_loop(state.clone());
    services::ytauth::spawn_sync_loop(state.clone());
    services::importers::spawn_lastfm_sync(state.clone());
    services::spotify::spawn_export_watcher(state.clone());
    services::cache::spawn_purge_loop(state.db.clone());
    state.audio_cache.load().await;
    state.audio_cache.spawn_purge_loop();

    let app = routes::router(state.clone());

    if cfg.tailscale.embedded {
        start_tailnet(state.clone()).await;
    } else {
        *state.tailscale.lock().await = tailscale::host_status(cfg.bind).await;
    }

    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!("listening on http://{}", cfg.bind);
    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

#[cfg(feature = "embedded-tailscale")]
async fn start_tailnet(state: Arc<AppState>) {
    *state.tailscale.lock().await = tailscale::TailscaleInfo {
        mode: "embedded".into(),
        hint: Some("Joining your tailnet…".into()),
        ..Default::default()
    };
    let cfg = state.cfg.tailscale.clone();
    let shared = state.tailscale.clone();
    let bind = state.cfg.bind;
    tokio::spawn(async move {
        if let Err(e) = tailscale::embedded::run(cfg, bind, shared.clone()).await {
            tracing::error!("embedded tailscale failed: {e:#}");
            let mut i = shared.lock().await;
            i.online = false;
            i.hint = Some(format!("Embedded Tailscale failed: {e:#}"));
        }
    });
}

#[cfg(not(feature = "embedded-tailscale"))]
async fn start_tailnet(state: Arc<AppState>) {
    tracing::warn!("TS_EMBEDDED=1 but this build lacks the `embedded-tailscale` feature (see README); falling back to host tailscaled");
    *state.tailscale.lock().await = tailscale::host_status(state.cfg.bind).await;
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
