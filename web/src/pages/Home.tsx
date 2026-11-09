import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { api, isNotConnected, type Shelf, type Status } from "../lib/api";
import { ShelfView } from "../components/Shelf";
import ProfileToggle from "../components/ProfileToggle";
import { useProfile } from "../lib/profile";
import { fetchStatus, loadStatus, useStatus, useYoutubeOn, youtubeOn, sourceOn } from "../lib/serverStatus";
import { randomFolderTracks, useMusicFolder } from "../lib/localFolder";

export default function Home() {
  const [shelves, setShelves] = useState<Shelf[] | null>(null);
  const [personal, setPersonal] = useState<Shelf[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [offline, setOffline] = useState(false);
  // Infinite scroll, like YouTube Music's own home page: the token for the next shelves, fetched
  // only when you scroll near the bottom (one YouTube call per page, cached on the server).
  const [next, setNext] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [moreError, setMoreError] = useState(false);
  // Bumped by a refresh so a page still loading for the old feed is dropped.
  const generation = useRef(0);
  const sentinel = useRef<HTMLDivElement>(null);

  // The YouTube sign-in expired (YouTube answers as if signed out) and refreshing it failed:
  // why, for the warning next to Refresh. Checked after the feed loads, since that request is
  // what tells the server whether the sign-in still works.
  const [tokenWarn, setTokenWarn] = useState<string | null>(null);
  const checkToken = () =>
    fetchStatus()
      .then((s) => setTokenWarn(youtubeOn(s) ? tokenProblem(s.ytmusic) : null))
      .catch(() => {});
  const youtube = useYoutubeOn();

  const [refreshing, setRefreshing] = useState(false);
  const load = (refresh = false) => {
    const gen = ++generation.current;
    if (refresh) setRefreshing(true);
    setMoreError(false);
    api
      .home(refresh)
      .then((r) => {
        if (gen !== generation.current) return;
        setShelves(r.shelves);
        setNext(r.continuation ?? null);
      })
      .catch((e) => (isNotConnected(e) ? setOffline(true) : setError(e.message)))
      .finally(() => {
        setRefreshing(false);
        checkToken();
      });
    // Personal mixes come from YouTube; with YouTube off the home feed itself is built from your libraries.
    loadStatus().then((s) => {
      if (gen !== generation.current) return;
      if (youtubeOn(s)) api.discover().then((r) => setPersonal(r.shelves)).catch(() => {});
      else setPersonal([]);
    });
  };
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadMore = (token: string) => {
    const gen = generation.current;
    setLoadingMore(true);
    setMoreError(false);
    api
      .homeMore(token)
      .then((r) => {
        if (gen !== generation.current) return;
        setShelves((s) => [...(s ?? []), ...r.shelves]);
        // A page with nothing on it ends the feed rather than asking forever.
        setNext(r.shelves.length > 0 ? r.continuation ?? null : null);
      })
      .catch(() => gen === generation.current && setMoreError(true))
      .finally(() => setLoadingMore(false));
  };

  // Watch the end of the list inside the scrolling pane, starting a little before it shows. The
  // observer is rebuilt after each page, so a page too short to fill the screen loads the next.
  useEffect(() => {
    const el = sentinel.current;
    if (!el || !next || loadingMore || moreError) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          io.disconnect();
          loadMore(next);
        }
      },
      { root: el.closest(".content"), rootMargin: "0px 0px 900px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [next, loadingMore, moreError]);

  const fresh = useProfile((s) => s.active === "fresh");
  return (
    <div className="page">
      <div className="row between home-head">
        <h1>Listen now</h1>
        <div className="row gap">
          <ProfileToggle compact />
          {tokenWarn && youtube && (
            <Link to="/settings" className="token-warn" title={tokenWarn} aria-label={`Token Expired. ${tokenWarn}`}>
              <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden>
                <path d="M10.3 3.9 1.9 18a2 2 0 0 0 1.7 3h16.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" fill="#e5383b" />
                <path d="M12 9v4.5M12 17h.01" stroke="#fff" strokeWidth="2.2" strokeLinecap="round" />
              </svg>
              Token Expired
            </Link>
          )}
          <button className="btn ghost small" disabled={refreshing} onClick={() => load(true)} title={youtube ? "Get a fresh feed from YouTube Music" : "Build the feed again from your libraries"}>
            {refreshing ? "Refreshing…" : "Refresh"}
          </button>
        </div>
      </div>
      {fresh && (
        <p className="fresh-note small">
          <strong>Fresh start</strong> — you're listening as a brand-new user: no history, not signed in to YouTube, nothing
          scrobbled or added to your YouTube history. Recommendations build up from what you play here.
        </p>
      )}
      {personal.map((s) => (
        <ShelfView key={`p-${s.title}`} shelf={s} />
      ))}
      <FolderShelf />
      {error && <p className="error">{error}</p>}
      {offline && <p className="muted">Your mixes and recommendations will appear here once you're connected.</p>}
      {!shelves && !error && !offline && <div className="spinner" />}
      {shelves?.map((s, i) => <ShelfView key={`${s.title}-${i}`} shelf={s} />)}
      <div ref={sentinel} aria-hidden />
      {loadingMore && <div className="spinner" />}
      {moreError && (
        <p className="muted">
          Couldn't load more.{" "}
          <button className="link-btn" onClick={() => next && loadMore(next)}>
            Try again
          </button>
        </p>
      )}
      {shelves && shelves.length === 0 && !youtube && (
        <p className="muted">
          YouTube is off, so Home is built from your libraries and what you play. Connect Plex or Jellyfin, or add a music
          folder, in <Link to="/settings#sources">Settings</Link>.
        </p>
      )}
      {shelves && shelves.length < 3 && !fresh && youtube && (
        <p className="muted">
          Tip: add your YouTube Music cookie in <Link to="/settings">Settings</Link> for personalised mixes, Quick picks and
          your Supermix.
        </p>
      )}
    </div>
  );
}

const BROWSER_NAMES: Record<string, string> = {
  firefox: "Firefox",
  chrome: "Chrome",
  chromium: "Chromium",
  brave: "Brave",
  edge: "Edge",
  vivaldi: "Vivaldi",
  opera: "Opera",
};

/**
 * Why the YouTube sign-in needs attention, or null: YouTube treats it as signed out (it expired)
 * and the last refresh from the browser failed (or it was pasted by hand, which can't refresh).
 */
function tokenProblem(y: Status["ytmusic"]): string | null {
  if (!y.authenticated || !y.expiredSince) return null;
  const browser = y.source?.startsWith("browser:") ? y.source.slice(8).split(":")[0] : null;
  if (browser && !y.syncError) return null; // the next refresh from the browser may still fix it
  const since = new Date(y.expiredSince * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
  const refresh = browser
    ? `Refreshing it from ${BROWSER_NAMES[browser] ?? browser} on the server failed too: ${y.syncError}`
    : "It was pasted in by hand, so it can't refresh itself.";
  const fix = y.signinBrowserUrl
    ? "Click to open Settings, then sign in again in the server's browser."
    : "Click to sign in again in Settings.";
  return (
    `Your YouTube Music sign-in has expired: since ${since}, YouTube treats the app as a signed-out visitor, ` +
    `so Home shows general picks instead of your mixes, and your likes and history aren't used. ${refresh} ` +
    fix
  );
}

/** Songs from the music folder on this device (the server can't see it, so this is built here). */
function FolderShelf() {
  const ready = useMusicFolder((s) => s.state === "ready");
  const tracks = useMusicFolder((s) => s.tracks);
  const on = sourceOn(useStatus(), "local");
  // Picked once per visit (and when the folder's song count changes), not on every render.
  const shelf = useMemo<Shelf | null>(() => {
    if (!ready || !on || !tracks) return null;
    const items = randomFolderTracks(20).map((t) => ({ kind: "song" as const, title: t.title, subtitle: t.artist, videoId: t.videoId, track: t }));
    return items.length ? { title: "From your music folder", items } : null;
  }, [ready, on, tracks]);
  return shelf ? <ShelfView shelf={shelf} /> : null;
}
