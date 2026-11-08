//! YouTube Music sign-in by importing the session from a browser on the server machine.
//!
//! The user signs in to Google normally in their own browser; we read the resulting
//! music.youtube.com cookies from that browser's cookie store (via yt-dlp) and, optionally,
//! keep re-importing them so the server's copy never goes stale when the browser rotates them.

use std::time::Duration;

use anyhow::bail;

use crate::{
    models::now,
    services::ytmusic::{cookie_header, ACCOUNT_KEY, BROWSERS, COOKIE_KEY, COOKIE_SOURCE_KEY, COOKIE_SYNCED_KEY},
    state::{AppState, SharedState},
};

const SYNC_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

pub struct Imported {
    pub browser: String,
    pub cookie: String,
}

/// Find a signed-in YouTube session in the given browser, or the first browser that has one.
pub async fn find_session(state: &AppState, only: Option<&str>) -> anyhow::Result<Imported> {
    let list: Vec<&str> = match only {
        Some(b) => vec![b],
        None => BROWSERS.to_vec(),
    };
    let mut not_signed_in = Vec::new();
    let mut other_errors = Vec::new();
    for b in list {
        match state.ytdlp.browser_cookies(b).await {
            Ok(jar) => match cookie_header(&jar) {
                Some(cookie) => return Ok(Imported { browser: b.to_string(), cookie }),
                None => not_signed_in.push(b),
            },
            Err(e) => {
                let msg = format!("{e:#}");
                // Browsers that simply aren't installed aren't worth reporting.
                if !msg.contains("could not find") && !msg.contains("no cookie store found") {
                    other_errors.push(msg);
                }
            }
        }
    }
    if !not_signed_in.is_empty() {
        bail!(
            "Found {} on the server computer, but it isn't signed in to YouTube. Sign in at music.youtube.com in that browser, then try again.",
            not_signed_in.join(", ")
        );
    }
    if !other_errors.is_empty() {
        bail!("Couldn't read browser cookies: {}", other_errors.join("; "));
    }
    bail!("No supported browser (Firefox, Chrome, Chromium, Brave, Edge, Vivaldi, Opera) found on the server computer.")
}

pub async fn save(state: &AppState, cookie: &str, source: &str) -> anyhow::Result<()> {
    state.yt.reset_session_state().await;
    state.db.set_setting(COOKIE_KEY, cookie).await?;
    state.db.set_setting(COOKIE_SOURCE_KEY, source).await?;
    state.db.set_setting(COOKIE_SYNCED_KEY, &now().to_string()).await
}

/// Try a cookie and return the account it signs in as. The previous cookie is restored unless
/// the new one really is signed in.
pub async fn verify(state: &AppState, cookie: &str) -> anyhow::Result<String> {
    let previous = state.db.get_setting(COOKIE_KEY).await?;
    state.db.set_setting(COOKIE_KEY, cookie).await?;
    let result = state.yt.account_name().await;
    match result {
        Ok(Some(account)) => Ok(account),
        other => {
            match previous {
                Some(p) => state.db.set_setting(COOKIE_KEY, &p).await?,
                None => state.db.call(|c| c.execute("DELETE FROM settings WHERE key='ytmusic.cookie'", [])).await.map(|_| ())?,
            }
            match other {
                Err(e) => bail!("YouTube didn't accept this sign-in: {e:#}"),
                _ => bail!("This sign-in has expired or was signed out — YouTube treats it as a logged-out visitor."),
            }
        }
    }
}

/// Import and verify a browser session; on failure the previous cookie is kept.
/// Returns (browser, account name).
pub async fn import(state: &AppState, only: Option<&str>, keep_in_sync: bool) -> anyhow::Result<(String, String)> {
    let found = find_session(state, only).await?;
    let account = verify(state, &found.cookie)
        .await
        .map_err(|e| anyhow::anyhow!("Found a YouTube sign-in in {}, but: {e:#}", found.browser))?;
    let source = if keep_in_sync { format!("browser:{}", found.browser) } else { "manual".to_string() };
    save(state, &found.cookie, &source).await?;
    state.db.set_setting(ACCOUNT_KEY, &account).await?;
    let _ = state.db.call(|c| c.execute("DELETE FROM settings WHERE key=?1", [SYNC_ERROR_KEY])).await;
    tracing::info!("imported YouTube Music sign-in ({account}) from {}", found.browser);
    Ok((found.browser, account))
}

/// Why the last refresh from the browser didn't work (cleared when one does, and on sign-out).
pub const SYNC_ERROR_KEY: &str = "ytmusic.cookie_sync_error";

/// Periodically refresh a browser-sourced cookie from the same browser. Purely opportunistic:
/// nothing waits for it, and when the browser can't be read (e.g. it isn't installed where the
/// server runs) the saved sign-in simply keeps being used for as long as YouTube accepts it.
pub fn spawn_sync_loop(state: SharedState) {
    tokio::spawn(crate::services::ytlog::with_origin("cookie sync loop", async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            if let Err(e) = sync_once(&state).await {
                tracing::info!("couldn't refresh the YouTube Music sign-in from the browser ({e:#}); still using the saved one");
            }
            tokio::time::sleep(SYNC_EVERY).await;
        }
    }));
}

/// Refresh from the browser, remembering why it failed (shown in Settings) or clearing that.
pub async fn sync_once(state: &AppState) -> anyhow::Result<bool> {
    let result = sync_from_browser(state).await;
    let _ = match &result {
        Ok(_) => state.db.call(|c| c.execute("DELETE FROM settings WHERE key=?1", [SYNC_ERROR_KEY])).await.map(|_| ()),
        Err(e) => state.db.set_setting(SYNC_ERROR_KEY, &format!("{e:#}")).await,
    };
    result
}

async fn sync_from_browser(state: &AppState) -> anyhow::Result<bool> {
    let Some(source) = state.db.get_setting(COOKIE_SOURCE_KEY).await? else { return Ok(false) };
    let Some(browser) = source.strip_prefix("browser:") else { return Ok(false) };
    let found = find_session(state, Some(browser)).await?;
    let current = state.db.get_setting(COOKIE_KEY).await?;
    if current.as_deref() != Some(found.cookie.as_str()) {
        // Only replace a working cookie with one that's actually signed in.
        let account = verify(state, &found.cookie).await?;
        save(state, &found.cookie, &source).await?;
        state.db.set_setting(ACCOUNT_KEY, &account).await?;
        tracing::info!("refreshed YouTube Music sign-in from {browser}");
    } else {
        state.db.set_setting(COOKIE_SYNCED_KEY, &now().to_string()).await?;
    }
    Ok(true)
}
