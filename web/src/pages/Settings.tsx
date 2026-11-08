import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import QRCode from "qrcode";
import { api, type PlexServer, type Prefs, type Status, type YtLog, type YtLogWindow } from "../lib/api";
import { useStore } from "../lib/store";
import { tailnetLogout, useTailnet } from "../lib/tailnet";
import {
  detectConnection,
  rememberServerStatus,
  setTailnetConfig,
  useConnection,
  useTransportReady,
} from "../lib/transport";
import TailnetSetup from "../components/TailnetSetup";
import ProfileToggle from "../components/ProfileToggle";
import { resetFresh, useProfile } from "../lib/profile";
import { CROSSFADE_MAX, crossfadeSupported, getCrossfade, setCrossfade } from "../lib/player";
import { getQualityPref, setQualityPref, type QualityPref } from "../lib/quality";

function Section({
  title,
  state,
  id,
  children,
}: {
  title: string;
  state?: "ok" | "off" | "warn";
  id?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="panel" id={id}>
      <h2>
        {state && <span className={`dot ${state}`} />} {title}
      </h2>
      {children}
    </section>
  );
}

const GOOGLE_SIGN_IN =
  "https://accounts.google.com/ServiceLogin?service=youtube&passive=true&continue=" +
  encodeURIComponent("https://music.youtube.com/");

const BROWSER_NAMES: Record<string, string> = {
  firefox: "Firefox",
  chrome: "Chrome",
  chromium: "Chromium",
  brave: "Brave",
  edge: "Edge",
  vivaldi: "Vivaldi",
  opera: "Opera",
};
const browserName = (source?: string | null) => {
  const b = source?.startsWith("browser:") ? source.slice(8).split(":")[0] : null;
  return b ? (BROWSER_NAMES[b] ?? b) : null;
};

function ago(ts?: number | null) {
  if (!ts) return "";
  const m = Math.round((Date.now() / 1000 - ts) / 60);
  return m < 1 ? "just now" : m < 60 ? `${m} min ago` : m < 1440 ? `${Math.round(m / 60)} h ago` : `${Math.round(m / 1440)} d ago`;
}

function YouTubeMusic({ status, reload }: { status: Status; reload: () => void }) {
  const toast = useStore((s) => s.toastMsg);
  const yt = status.ytmusic;
  const [cookie, setCookie] = useState("");
  const [busy, setBusy] = useState<"import" | "paste" | "sync" | null>(null);
  const [opened, setOpened] = useState(false);
  const [keepInSync, setKeepInSync] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const autoTried = useRef(false);
  const onServerComputer = ["localhost", "127.0.0.1", "[::1]"].includes(location.hostname);

  const importFromBrowser = async (quiet = false) => {
    setBusy("import");
    setError(null);
    try {
      const r = await api.importYtFromBrowser(keepInSync);
      toast(`Signed in to YouTube Music as ${r.account} (from ${BROWSER_NAMES[r.browser.split(":")[0]] ?? r.browser})`);
      setOpened(false);
      reload();
    } catch (e) {
      if (!quiet) setError((e as Error).message);
    } finally {
      setBusy(null);
    }
  };

  // After the Google tab was opened, try importing as soon as the user comes back to this tab.
  useEffect(() => {
    if (!opened) return;
    const onVisible = () => {
      if (document.visibilityState === "visible" && !autoTried.current) {
        autoTried.current = true;
        importFromBrowser(true);
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opened]);

  const synced = browserName(yt.source);

  return (
    <Section title="YouTube Music" state={yt.authenticated ? "ok" : "warn"}>
      <p className="muted small">
        Search, radio and public playlists work without an account. Signing in adds your personalised home feed, liked songs
        import, and reporting plays to your YouTube history.
      </p>

      {yt.authenticated && !yt.activeNow && (
        <p className="small warn-text">Not used right now — Fresh start is on, so YouTube sees a brand-new anonymous listener.</p>
      )}
      {yt.authenticated ? (
        <>
          <p className="small">
            Signed in{yt.account ? <> as <strong>{yt.account}</strong></> : null}
            {synced ? (
              <>
                {" "}
                · kept in sync with {synced} on the server computer
                {yt.syncedAt ? ` (checked ${ago(yt.syncedAt)})` : ""}
              </>
            ) : (
              " · from a pasted cookie"
            )}
            .
          </p>
          {synced && yt.syncError && (
            <p className="small muted">
              Couldn't refresh it from {synced} on this server ({yt.syncError}). That's fine while it keeps working: the saved
              sign-in is still used, and YouTube rejecting it is what would need a new sign-in.
            </p>
          )}
          <div className="row gap wrap">
            {synced && (
              <button
                className="btn ghost small"
                disabled={busy === "sync"}
                onClick={async () => {
                  setBusy("sync");
                  try {
                    await api.syncYt();
                    toast("Sign-in refreshed");
                    reload();
                  } catch (e) {
                    toast((e as Error).message);
                  } finally {
                    setBusy(null);
                  }
                }}
              >
                {busy === "sync" ? "Syncing…" : "Sync now"}
              </button>
            )}
            <button className="btn ghost small danger" onClick={() => api.deleteYtCookie().then(reload)}>
              Sign out
            </button>
          </div>
        </>
      ) : (
        <>
          <div className="setup-step">
            {!onServerComputer && (
              <p className="small warn-text">
                Do this on the computer that runs the server: the server picks up the sign-in from that computer's browser.
              </p>
            )}
            <div className="row gap wrap">
              <a
                className="btn"
                href={GOOGLE_SIGN_IN}
                target="_blank"
                rel="noreferrer"
                onClick={() => {
                  autoTried.current = false;
                  setOpened(true);
                }}
              >
                Sign in with Google
              </a>
              <button className="btn ghost" disabled={busy === "import"} onClick={() => importFromBrowser()}>
                {busy === "import" ? "Looking for your sign-in…" : opened ? "I've signed in — finish" : "Already signed in? Import"}
              </button>
            </div>
            <p className="muted small">
              {opened
                ? "Sign in on the Google page that opened, then come back here — this finishes automatically."
                : "Opens Google's sign-in page in a new tab. Afterwards the server reads your YouTube sign-in from this computer's browser (Firefox, Chrome, Chromium, Brave, Edge, Vivaldi or Opera)."}
            </p>
            <label className="switch">
              <input type="checkbox" checked={keepInSync} onChange={(e) => setKeepInSync(e.target.checked)} />
              <span>Keep in sync with the browser (so the sign-in doesn't expire)</span>
            </label>
            {error && <p className="error small">{error}</p>}
          </div>

          <details className="small">
            <summary className="muted">Or paste a cookie instead (e.g. from another computer)</summary>
            <ol className="muted">
              <li>Open music.youtube.com in a desktop browser while signed in.</li>
              <li>Open DevTools → Network, click any request to music.youtube.com/youtubei/v1/…</li>
              <li>In Request Headers, copy the full value of the <code>cookie</code> header and paste it here.</li>
            </ol>
            <textarea
              className="input"
              rows={3}
              placeholder="VISITOR_INFO1_LIVE=…; SAPISID=…; __Secure-3PAPISID=…"
              value={cookie}
              onChange={(e) => setCookie(e.target.value)}
            />
            <button
              className="btn ghost"
              disabled={!cookie.trim() || busy === "paste"}
              onClick={async () => {
                setBusy("paste");
                try {
                  const r = await api.setYtCookie(cookie);
                  setCookie("");
                  toast(r.account ? `Signed in as ${r.account}` : "YouTube Music connected");
                  reload();
                } catch (e) {
                  toast((e as Error).message);
                } finally {
                  setBusy(null);
                }
              }}
            >
              {busy === "paste" ? "Checking…" : "Save cookie"}
            </button>
          </details>
          <p className="muted small">Your sign-in is stored only on your server.</p>
        </>
      )}
    </Section>
  );
}

function Plex({ status, reload }: { status: Status; reload: () => void }) {
  const toast = useStore((s) => s.toastMsg);
  const [pin, setPin] = useState<{ id: number; code: string; authUrl: string } | null>(null);
  const [servers, setServers] = useState<PlexServer[] | null>(null);
  const [manual, setManual] = useState({ url: "", token: "" });

  // Poll the PIN until the user approves it on plex.tv.
  useEffect(() => {
    if (!pin) return;
    const t = setInterval(async () => {
      try {
        if ((await api.plexPinCheck(pin.id)).authorized) {
          setPin(null);
          setServers(await api.plexServers());
          reload();
        }
      } catch {
        /* keep polling */
      }
    }, 2000);
    return () => clearInterval(t);
  }, [pin, reload]);

  useEffect(() => {
    if (status.plex.loggedIn && !status.plex.connected) api.plexServers().then(setServers).catch(() => {});
  }, [status.plex.loggedIn, status.plex.connected]);

  const choose = async (uri: string, token: string) => {
    try {
      await api.plexSetServer(uri, token);
      toast("Plex server connected");
      setServers(null);
      reload();
    } catch (e) {
      toast((e as Error).message);
    }
  };

  return (
    <Section title="Plex / Plexamp" state={status.plex.connected ? "ok" : "off"}>
      <p className="muted small">
        When a track also exists in your Plex music library, it's streamed from Plex instead of YouTube (lossless if your
        files are).
      </p>
      {status.plex.connected ? (
        <div className="row gap wrap">
          <span className="small">{status.plex.url}</span>
          <button className="btn ghost small" onClick={() => api.plexServers().then(setServers).catch((e) => toast(e.message))}>
            Change server
          </button>
          <button className="btn ghost small danger" onClick={() => api.plexDisconnect().then(reload)}>
            Disconnect
          </button>
        </div>
      ) : (
        !pin && (
          <button
            className="btn"
            onClick={async () => {
              try {
                const p = await api.plexPin();
                setPin(p);
                window.open(p.authUrl, "_blank", "noopener");
              } catch (e) {
                toast((e as Error).message);
              }
            }}
          >
            Sign in with Plex
          </button>
        )
      )}
      {pin && (
        <p className="small">
          Waiting for approval… If no window opened, <a href={pin.authUrl} target="_blank" rel="noreferrer">open plex.tv</a>.
        </p>
      )}
      {servers && (
        <div className="server-list">
          {servers.map((s) => (
            <div key={s.clientIdentifier} className="server">
              <strong>{s.name}</strong> {s.owned ? "" : <span className="muted small">(shared)</span>}
              {s.connections.map((c) => (
                <button key={c.uri} className="btn ghost small conn" onClick={() => choose(c.uri, s.accessToken)}>
                  {c.local ? "Local" : c.relay ? "Relay" : "Remote"} · {c.uri.replace(/^https?:\/\//, "").slice(0, 48)}
                </button>
              ))}
            </div>
          ))}
          {servers.length === 0 && <p className="muted small">No servers found on this account.</p>}
        </div>
      )}
      <details className="small">
        <summary className="muted">Manual connection</summary>
        <div className="row gap wrap">
          <input className="input grow" placeholder="http://192.168.1.10:32400" value={manual.url} onChange={(e) => setManual({ ...manual, url: e.target.value })} />
          <input className="input grow" placeholder="X-Plex-Token" value={manual.token} onChange={(e) => setManual({ ...manual, token: e.target.value })} />
          <button className="btn" disabled={!manual.url || !manual.token} onClick={() => choose(manual.url, manual.token)}>
            Connect
          </button>
        </div>
      </details>
    </Section>
  );
}

function LastFm({ status, reload }: { status: Status; reload: () => void }) {
  const toast = useStore((s) => s.toastMsg);
  const [keys, setKeys] = useState({ apiKey: "", apiSecret: "" });
  const lf = status.lastfm;
  return (
    <Section title="Last.fm" state={lf.connected ? "ok" : "off"}>
      <p className="muted small">Scrobbles what you listen to and adds Last.fm's similar tracks to recommendations and autoplay.</p>
      {!lf.configured && (
        <>
          <p className="small">
            Create an API account at{" "}
            <a href="https://www.last.fm/api/account/create" target="_blank" rel="noreferrer">
              last.fm/api/account/create
            </a>{" "}
            (callback URL: <code>{location.origin}/oauth/lastfm</code>), then paste the key and secret.
          </p>
          <div className="row gap wrap">
            <input className="input grow" placeholder="API key" value={keys.apiKey} onChange={(e) => setKeys({ ...keys, apiKey: e.target.value })} />
            <input className="input grow" placeholder="Shared secret" value={keys.apiSecret} onChange={(e) => setKeys({ ...keys, apiSecret: e.target.value })} />
            <button className="btn" disabled={!keys.apiKey || !keys.apiSecret} onClick={() => api.setLastfmKeys(keys.apiKey, keys.apiSecret).then(reload)}>
              Save
            </button>
          </div>
        </>
      )}
      {lf.configured && !lf.connected && (
        <button
          className="btn"
          onClick={async () => {
            try {
              window.location.href = (await api.lastfmLogin(`${location.origin}/oauth/lastfm`)).url;
            } catch (e) {
              toast((e as Error).message);
            }
          }}
        >
          Connect Last.fm
        </button>
      )}
      {lf.connected && (
        <div className="row gap">
          <span>
            Scrobbling as <strong>{lf.username}</strong>
          </span>
          <button className="btn ghost small" onClick={() => api.lastfmDisconnect().then(reload)}>
            Disconnect
          </button>
        </div>
      )}
    </Section>
  );
}

/** Where the server picks up playlist exports (the Downloads folder it found, or one set here). */
function ExportFolder({ status, reload }: { status: Status; reload: () => void }) {
  const toast = useStore((s) => s.toastMsg);
  const [editing, setEditing] = useState(false);
  const [dir, setDir] = useState("");
  const dirs = status.spotify.exportDirs ?? [];
  const save = async (value: string) => {
    try {
      await api.playlists.spotifyExportDir(value);
      setEditing(false);
      reload();
    } catch (e) {
      toast((e as Error).message);
    }
  };
  return (
    <div className="small" style={{ marginBottom: 10 }}>
      {dirs.length ? (
        <>
          Playlist exports (CSV) are picked up from <code>{dirs.join("; ")}</code>
          {status.spotify.exportDirSet ? " (set here)" : ""}.{" "}
        </>
      ) : (
        <>The server found no Downloads folder to watch, so drop export files on the playlist page instead. </>
      )}
      {!editing ? (
        <button
          className="link-btn"
          onClick={() => {
            setDir(dirs.join("; "));
            setEditing(true);
          }}
        >
          Change
        </button>
      ) : (
        <div className="row gap wrap" style={{ marginTop: 6 }}>
          <input
            className="input grow"
            placeholder="Folder on the server, e.g. /mnt/c/Users/you/Downloads"
            value={dir}
            onChange={(e) => setDir(e.target.value)}
          />
          <button className="btn small" onClick={() => save(dir)}>
            Save
          </button>
          {status.spotify.exportDirSet && (
            <button className="btn ghost small" onClick={() => save("")}>
              Use Downloads
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/** Spotify playlists (Playlists page): where their full lists (exports from Chosic) are picked up. */
function Spotify({ status, reload }: { status: Status; reload: () => void }) {
  return (
    <Section title="Spotify playlists" id="spotify">
      <p className="muted small">
        Spotify playlists imported on the Playlists page show their first 100 songs from Spotify's public page. For
        longer ones, “Sync now” on the playlist copies its link and opens Chosic's playlist exporter: paste the link there
        and download the CSV, and the server picks the download up by itself and fills in the rest.
      </p>
      <ExportFolder status={status} reload={reload} />
    </Section>
  );
}

function ListeningProfile() {
  const p = useProfile();
  const fresh = p.active === "fresh";
  return (
    <Section title="Fresh start" state={fresh ? "ok" : "off"}>
      <p className="small muted">
        Listen as a brand-new user with no history — handy for discovering music outside your usual taste, or for guests. While
        it's on, YouTube Music sees an anonymous new listener (not your Google account), the app keeps a separate play history
        and queue that its recommendations learn from, and nothing is scrobbled to Last.fm or added to your YouTube history.
        Your library, playlists and settings stay the same.
      </p>
      <ProfileToggle />
      {p.loaded && (p.freshPlays > 0 || fresh) && (
        <div className="row gap wrap">
          <span className="small muted">
            Fresh start history: {p.freshPlays} play{p.freshPlays === 1 ? "" : "s"}
            {p.freshStartedAt ? ` since ${new Date(p.freshStartedAt * 1000).toLocaleDateString()}` : ""}.
          </span>
          <button
            className="btn ghost small"
            onClick={() => confirm("Forget everything the Fresh start profile has learned and start over?") && resetFresh()}
          >
            Reset to brand new
          </button>
        </div>
      )}
    </Section>
  );
}

function Preferences({ status, reload }: { status: Status; reload: () => void }) {
  const [p, setP] = useState<Prefs>(status.prefs);
  const update = async (patch: Partial<Prefs>) => {
    const np = { ...p, ...patch };
    setP(np);
    await api.setPrefs(np);
    reload();
  };
  return (
    <Section title="Playback">
      <label className="switch block">
        <input type="checkbox" checked={p.preferPlex} onChange={(e) => update({ preferPlex: e.target.checked })} />
        <span>Prefer Plex when the track is in my library</span>
      </label>
      <label className="switch block">
        <input type="checkbox" checked={p.scrobble} onChange={(e) => update({ scrobble: e.target.checked })} />
        <span>Scrobble to Last.fm</span>
      </label>
      <label className="switch block">
        <input type="checkbox" checked={p.reportYoutube} onChange={(e) => update({ reportYoutube: e.target.checked })} />
        <span>Add played tracks to my YouTube watch history (improves YouTube Music recommendations)</span>
      </label>
      <label className="switch block">
        <input type="checkbox" checked={p.loveOnLastfm} onChange={(e) => update({ loveOnLastfm: e.target.checked })} />
        <span>♥ also loves / un-loves the song on Last.fm</span>
      </label>
      <label className="field">
        <span className="small">Daily Last.fm loves sync</span>
        <select
          className="input"
          value={p.lastfmSync}
          onChange={(e) => update({ lastfmSync: e.target.value as Prefs["lastfmSync"] })}
        >
          <option value="off">Off</option>
          <option value="pull">Loved tracks → library</option>
          <option value="both">Two-way: library ↔ loved tracks</option>
        </select>
      </label>
      <AudioCache status={status} hours={p.audioCacheHours} onChange={(h) => update({ audioCacheHours: h })} />
      <AudioQuality />
      <Crossfade />
    </Section>
  );
}

/** `audioCacheHours` value for copies that never expire. */
const CACHE_FOREVER = -1;

const CACHE_HOURS = [
  [0, "Off"],
  [1, "1 hour"],
  [3, "3 hours"],
  [6, "6 hours"],
  [12, "12 hours"],
  [24, "1 day"],
  [72, "3 days"],
  [168, "1 week"],
  [720, "30 days"],
  [CACHE_FOREVER, "Forever"],
] as const;

/** Server-wide: MP3 copies of fully loaded YouTube tracks, kept in the server's data folder. */
function AudioCache({ status, hours, onChange }: { status: Status; hours: number; onChange: (h: number) => void }) {
  const c = status.audioCache;
  const options: [number, string][] = CACHE_HOURS.map(([h, l]) => [h, l]);
  // A value set some other way (e.g. directly in the database) still shows up.
  if (!options.some(([h]) => h === hours)) options.push([hours, `${hours} hours`]);
  const size = c.bytes >= 1e9 ? `${(c.bytes / 1e9).toFixed(1)} GB` : `${Math.round(c.bytes / 1e6)} MB`;
  return (
    <label className="field">
      <span className="small">Keep MP3 copies of YouTube plays</span>
      <select className="input" value={hours} disabled={!c.available} onChange={(e) => onChange(Number(e.target.value))}>
        {options.map(([h, l]) => (
          <option key={h} value={h}>
            {l}
          </option>
        ))}
      </select>
      <span className="small muted">
        {c.available
          ? `Once a song from YouTube has fully loaded at high quality, the server saves it as a tagged MP3 (title, artist, album, cover) and plays that copy instead, shown as “Cached”. ${c.files} saved now (${size}).` +
            (hours === CACHE_FOREVER
              ? " Copies are never deleted automatically; remove files from the server's data/audio-cache folder to free space."
              : "")
          : "Needs ffmpeg on the server (install it, or set YTP_FFMPEG), then restart the server."}
      </span>
    </label>
  );
}

/** Per-device: it's about this browser's connection to the server. */
function AudioQuality() {
  const [pref, setPref] = useState<QualityPref>(getQualityPref);
  const net = useStore((s) => s.netKbps);
  const budget = useStore((s) => s.qualityBudget);
  const speed = net == null ? null : net >= 1000 ? `${(net / 1000).toFixed(1)} Mbps` : `${net} kbps`;
  return (
    <label className="field">
      <span className="small">
        Audio quality <span className="muted">· this device only</span>
      </span>
      <select
        className="input"
        value={pref}
        onChange={(e) => {
          const v = e.target.value as QualityPref;
          setPref(v);
          setQualityPref(v);
        }}
      >
        <option value="auto">Automatic · adjusts to the connection to your server</option>
        <option value="high">High · original quality (YouTube ~130 kbps, Plex file as-is)</option>
        <option value="normal">Normal · about 96 kbps (YouTube ~66 kbps)</option>
        <option value="low">Low · about 50–64 kbps, for very slow connections</option>
      </select>
      <span className="small muted">
        {speed ? `Measured connection to the server: about ${speed}. ` : "The connection to the server is measured as songs load. "}
        {pref === "auto" && (budget == null ? "Streaming at full quality." : `New songs are limited to about ${budget} kbps.`)}
        {pref === "auto" && " A stall mid-song switches it to lower quality straight away."}
      </span>
    </label>
  );
}

/** Per-device: it's this browser's audio that fades. */
function Crossfade() {
  const [secs, setSecs] = useState(getCrossfade);
  if (!crossfadeSupported) {
    return (
      <p className="small muted">
        Crossfade isn't available in this browser (iPhone and iPad don't let web pages change the volume). The next song
        is still downloaded ahead of time.
      </p>
    );
  }
  return (
    <label className="field">
      <span className="small">
        Crossfade between songs: <strong>{secs === 0 ? "off" : `${secs} s`}</strong>
        <span className="muted"> · this device only</span>
      </span>
      <input
        type="range"
        className="crossfade-range"
        min={0}
        max={CROSSFADE_MAX}
        step={1}
        value={secs}
        onChange={(e) => {
          const v = Number(e.target.value);
          setSecs(v);
          setCrossfade(v);
        }}
      />
      <span className="small muted">
        The next song in the queue is loaded once the current one has finished downloading, so songs change without a pause
        {secs > 0 ? `, and the last ${secs} s of each song fade into the next` : ""}.
      </span>
    </label>
  );
}

/**
 * The ytp server's own presence on your tailnet. This is about the *server*, not this browser:
 * it's what makes the server reachable from your devices when you're away from home.
 */
function ServerSection({ status, reload }: { status: Status; reload: () => void }) {
  const ts = status.tailscale;
  const joining = ts.mode !== "none" && !ts.online;
  // While the server is still joining, refresh so its sign-in link / state update live.
  useEffect(() => {
    if (!joining) return;
    const t = setInterval(reload, 3000);
    return () => clearInterval(t);
  }, [joining, reload]);

  const via = ts.mode === "embedded" ? "built-in Tailscale" : "this machine's Tailscale";
  return (
    <Section title="Server on Tailscale" state={ts.online ? "ok" : ts.mode === "none" ? "off" : "warn"}>
      {ts.online ? (
        <p className="small">
          Your server is on your tailnet as <code>{ts.dnsName ?? "?"}</code>
          {ts.ips[0] ? ` (${ts.ips[0]})` : ""}, via {via}. Your devices can reach it at <code>{ts.url ?? "—"}</code>.
        </p>
      ) : ts.mode === "none" ? (
        <p className="small">Your server isn't on your tailnet, so devices can't reach it away from home.</p>
      ) : (
        <p className="small">
          {ts.state === "NeedsLogin"
            ? "The server is waiting to be signed in to your tailnet."
            : ts.state === "NeedsMachineAuth"
              ? "The server is signed in and waiting for approval in the Tailscale admin console."
              : "The server is joining your tailnet…"}
        </p>
      )}
      {ts.authUrl && !ts.online && (
        <a className="btn" href={ts.authUrl} target="_blank" rel="noreferrer">
          Sign the server in to Tailscale
        </a>
      )}
      {ts.hint && <p className={`small ${ts.online ? "muted" : "warn-text"}`}>{ts.hint}</p>}
    </Section>
  );
}

/**
 * How *this browser* reaches the server: directly at the page's address whenever that works,
 * otherwise (optionally) through its own in-page Tailscale connection — "away-from-home access".
 */
function DeviceSection({ status }: { status: Status | null }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const conn = useConnection((s) => s.conn);
  const fallback = useConnection((s) => s.tailnet);
  const directAvailable = useConnection((s) => s.directAvailable);
  const pageWithoutServer = useConnection((s) => s.pageWithoutServer);
  const ready = useTransportReady();
  const tn = useTailnet();
  const toast = useStore((s) => s.toastMsg);
  const [enabling, setEnabling] = useState(false);
  const [checking, setChecking] = useState(false);
  const appUrl = location.origin;
  const serverOnTailnet = status?.tailscale.online;

  useEffect(() => {
    if (canvas.current) QRCode.toCanvas(canvas.current, appUrl, { width: 160, margin: 1 }).catch(() => {});
  }, [appUrl, ready]);

  // Setup is shown when the user turns it on, or when this device has no working route at all.
  const needsSetup = conn?.mode === "tailnet" ? tn.phase !== "running" : !ready && !fallback && directAvailable === false;
  const showSetup = enabling || needsSetup;
  const state = ready ? "ok" : conn ? "warn" : "off";

  return (
    <Section title="This device" state={state} id="connection">
      <p className="small">
        {conn?.mode === "direct" ? (
          <>
            Connected <strong>directly</strong> to your server at {location.host}.
          </>
        ) : conn?.mode === "tailnet" ? (
          <>
            Connected <strong>through Tailscale</strong> — {location.host} can't reach your server from here. Switches back to
            direct automatically when it can.
          </>
        ) : directAvailable === false && pageWithoutServer ? (
          <>
            This page loads from {location.host}, but the <strong>ytp server behind it isn't answering</strong> — it's
            probably not running.{" "}
            {location.port === "5173" && (
              <>
                You're on the Vite dev server, which forwards <code>/api</code> to the ytp server (default{" "}
                <code>127.0.0.1:8080</code>, or <code>YTP_BACKEND</code>); start it with <code>scripts/dev.sh</code> or{" "}
                <code>cargo run</code> in <code>server/</code>.
              </>
            )}
          </>
        ) : directAvailable === false ? (
          <>Your server isn't reachable at {location.host} from here.</>
        ) : (
          <>Checking…</>
        )}
      </p>

      <div className="subsection">
        <div className="row between">
          <strong>Away-from-home access</strong>
          <span className={`pill ${fallback ? "on" : ""}`}>{fallback ? "On" : "Off"}</span>
        </div>
        <p className="small muted">
          When this device can't reach {location.host} (e.g. you're out), it signs in to your tailnet by itself — no
          Tailscale app or VPN needed — and connects to your server that way.
        </p>
        {fallback && !showSetup && (
          <p className="small">
            Uses <code>{fallback.server}</code>
            {tn.self ? (
              <>
                {" "}
                as <code>{tn.self.name.replace(/\.$/, "")}</code>
              </>
            ) : null}
            .
          </p>
        )}

        {showSetup ? (
          <TailnetSetup
            onSaved={() => {
              setEnabling(false);
              toast(directAvailable ? "Away-from-home access is on" : "Connected through Tailscale");
            }}
          />
        ) : !fallback ? (
          <>
            {status && !serverOnTailnet && (
              <p className="small warn-text">Put your server on Tailscale first (see “Server on Tailscale” above).</p>
            )}
            <button className="btn" disabled={!!status && !serverOnTailnet} onClick={() => setEnabling(true)}>
              Turn on
            </button>
          </>
        ) : (
          <div className="row gap wrap">
            <button className="btn ghost small" onClick={() => setEnabling(true)}>
              Change
            </button>
            <button
              className="btn ghost small danger"
              onClick={() => {
                tailnetLogout();
                setTailnetConfig(null);
                location.reload();
              }}
            >
              Turn off
            </button>
          </div>
        )}
      </div>

      <div className="row gap wrap">
        <button
          className="btn ghost small"
          disabled={checking}
          onClick={async () => {
            setChecking(true);
            const c = await detectConnection(true);
            setChecking(false);
            toast(c?.mode === "direct" ? "Server reachable directly" : c ? "Using Tailscale" : "Server not reachable");
          }}
        >
          {checking ? "Checking…" : "Check connection"}
        </button>
      </div>

      <details className="small">
        <summary className="muted">Open on your phone</summary>
        <div className="row gap wrap center">
          <canvas ref={canvas} className="qr" />
          <p className="muted" style={{ maxWidth: 360 }}>
            Scan to open <a href={appUrl}>{appUrl}</a>, then “Add to Home Screen” and turn on away-from-home access there.
            {!window.isSecureContext && <span className="warn-text"> This page isn't HTTPS, so it can't be installed.</span>}
          </p>
        </div>
      </details>
    </Section>
  );
}

function System({ status, reload }: { status: Status; reload: () => void }) {
  const toast = useStore((s) => s.toastMsg);
  const [busy, setBusy] = useState(false);
  return (
    <Section title="yt-dlp" state={status.ytdlp.version ? "ok" : "warn"}>
      <div className="small">
        Version {status.ytdlp.version ?? "not installed"} <span className="muted">· {status.ytdlp.path}</span>
      </div>
      <p className="muted small">YouTube changes often; if playback breaks, updating yt-dlp usually fixes it.</p>
      <button
        className="btn ghost"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            const r = await api.updateYtdlp();
            toast(`yt-dlp ${r.version ?? "updated"}`);
            reload();
          } catch (e) {
            toast((e as Error).message);
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy ? "Updating…" : "Update yt-dlp"}
      </button>
    </Section>
  );
}

function WindowStat({ label, w }: { label: string; w: YtLogWindow }) {
  return (
    <div className="log-stat">
      <div className="muted small">{label}</div>
      <div className="log-stat-num">{w.requests}</div>
      <div className="small">
        {w.requests > 0 && <>avg {w.avgMs} ms · max {(w.maxMs / 1000).toFixed(1)} s</>}
        {w.cacheHits > 0 && <span className="muted"> · {w.cacheHits} cached</span>}
      </div>
      {(w.timeouts > 0 || w.rateLimited > 0 || w.errors > 0) && (
        <div className="small error">
          {[
            w.timeouts && `${w.timeouts} timed out`,
            w.rateLimited && `${w.rateLimited} rate-limited`,
            w.errors && `${w.errors} failed`,
          ]
            .filter(Boolean)
            .join(" · ")}
        </div>
      )}
    </div>
  );
}

function CountList({ title, rows }: { title: string; rows: [string, number][] }) {
  if (!rows.length) return null;
  return (
    <div className="log-counts">
      <div className="small muted">{title}</div>
      {rows.map(([k, n]) => (
        <div key={k} className="row between small">
          <span className="ellipsis">{k}</span>
          <strong>{n}</strong>
        </div>
      ))}
    </div>
  );
}

/** Diagnostics: log every request the server makes to YouTube, to spot what's calling it too often. */
function YouTubeLog({ status, reload }: { status: Status; reload: () => void }) {
  const toast = useStore((s) => s.toastMsg);
  const [log, setLog] = useState<YtLog | null>(null);
  const on = status.prefs.logYoutube;
  const refresh = useRef(() => api.youtubeLog().then(setLog).catch(() => {})).current;

  useEffect(() => {
    refresh();
    if (!on) return;
    const t = setInterval(refresh, 5000);
    return () => clearInterval(t);
  }, [on, refresh]);

  const toggle = async (enabled: boolean) => {
    await api.setPrefs({ ...status.prefs, logYoutube: enabled });
    reload();
    refresh();
  };
  const download = async () => {
    try {
      const blob = await api.youtubeLogFile();
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = `youtube-${new Date().toISOString().slice(0, 19).replace(/:/g, "")}.log`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
    } catch (e) {
      toast((e as Error).message);
    }
  };
  const s = log?.stats;

  return (
    <Section title="YouTube request log" state={on ? "ok" : "off"}>
      <p className="muted small">
        Records every call the server makes to YouTube (API requests, play reports, yt-dlp, artwork), how long it took,
        and which screen or background job caused it. Use it to see whether something is calling YouTube too often.
      </p>
      <label className="switch block">
        <input type="checkbox" checked={on} onChange={(e) => toggle(e.target.checked)} />
        <span>Log YouTube requests</span>
      </label>
      {log?.path && <div className="small muted">File on the server: {log.path}</div>}
      {log && (
        <div className="small muted">
          {status.prefs.importPace === "original" ? (
            <>Imports run at the original speed: no spacing between requests and no retries.</>
          ) : (
            <>
              Imports and syncs space their requests ~{(log.backgroundGapMs / 1000).toFixed(1)} s apart
              {log.backgroundGapMs > 4000 * { normal: 1, slow: 2, very_slow: 4, original: 1 }[status.prefs.importPace]
                ? " (slowed down after YouTube refused requests)"
                : ""}
              .
            </>
          )}
        </div>
      )}

      {s && (on || log.recent.length > 0) && (
        <>
          <div className="log-stats">
            <WindowStat label="Last minute" w={s.lastMinute} />
            <WindowStat label="Last 10 min" w={s.last10Minutes} />
            <WindowStat label="Last hour" w={s.lastHour} />
            <div className="log-stat">
              <div className="muted small">Busiest minute</div>
              <div className="log-stat-num">{s.peakPerMinute}</div>
              <div className="small muted">requests</div>
            </div>
          </div>
          <div className="log-breakdown">
            <CountList title="By caller (last hour)" rows={s.byOrigin} />
            <CountList title="By request (last hour)" rows={s.byEndpoint} />
            <CountList title="Same request 3+ times (last hour)" rows={s.repeated} />
          </div>

          <details className="log-recent">
            <summary className="small">Recent requests ({log.recent.length})</summary>
            <div className="log-table">
              {log.recent.map((e, i) => (
                <div key={`${e.ts}-${i}`} className={`log-line ${["ok", "hit", "waited"].includes(e.outcome) ? "" : "bad"}`}>
                  <span className="muted">{new Date(e.ts).toLocaleTimeString()}</span>
                  <span>
                    {e.kind} <strong>{e.endpoint}</strong> {e.detail && <span className="muted">{e.detail}</span>}
                  </span>
                  <span>
                    {e.outcome}
                    {e.status ? ` ${e.status}` : ""} · {e.ms} ms{e.attempt > 1 ? ` · try ${e.attempt}` : ""}
                  </span>
                  <span className="muted">{e.origin}</span>
                  {e.error && <span className="error">{e.error}</span>}
                </div>
              ))}
            </div>
          </details>

          <div className="row gap">
            <button className="btn ghost" onClick={download}>
              Download log
            </button>
            <button className="btn ghost" onClick={() => api.clearYoutubeLog().then(refresh)}>
              Clear
            </button>
          </div>
        </>
      )}
    </Section>
  );
}

export default function Settings() {
  const ready = useTransportReady();
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [params] = useSearchParams();
  const toast = useStore((s) => s.toastMsg);
  const reload = useRef(() =>
    api
      .status()
      .then((s) => {
        setStatus(s);
        setError(null);
        rememberServerStatus(s);
      })
      .catch((e) => setError(e.message)),
  ).current;

  useEffect(() => {
    if (ready) reload();
  }, [ready, reload]);

  useEffect(() => {
    if (params.get("lastfm") === "connected") toast("Last.fm connected");
    if (params.get("lastfm") === "error") toast("Last.fm connection failed");
  }, [params, toast]);

  // Deep link from the "Set up connection" banner.
  useEffect(() => {
    if (location.hash === "#connection") document.getElementById("connection")?.scrollIntoView({ block: "start" });
  }, []);
  // Links to an account section (e.g. #spotify from Playlists), which appear once status loads.
  const loaded = !!status;
  useEffect(() => {
    const id = location.hash.slice(1);
    if (loaded && id && id !== "connection") document.getElementById(id)?.scrollIntoView({ block: "start" });
  }, [loaded]);

  return (
    <div className="page">
      <h1>Settings</h1>
      {!ready && <DeviceSection status={null} />}
      {ready && !status && !error && <div className="spinner" />}
      {ready && error && <p className="error">{error}</p>}
      {ready && status && (
        <>
          <YouTubeMusic status={status} reload={reload} />
          <Plex status={status} reload={reload} />
          <LastFm status={status} reload={reload} />
          <Spotify status={status} reload={reload} />
          <ListeningProfile />
          <Preferences status={status} reload={reload} />
          <ServerSection status={status} reload={reload} />
          <DeviceSection status={status} />
          <System status={status} reload={reload} />
          <YouTubeLog status={status} reload={reload} />
        </>
      )}
      {!ready && <p className="muted small">Account and playback settings appear here once you're connected to your server.</p>}
    </div>
  );
}
