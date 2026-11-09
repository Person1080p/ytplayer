import { useCallback, useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import {
  api,
  fmtTime,
  isLibraryOnlyId,
  libraryApi,
  libraryItem,
  SOURCE_LABELS,
  type ImportJob,
  type LikedEntry,
  type SourceId,
  type Status,
  type Track,
} from "../lib/api";
import { fetchStatus, sourceConnected, youtubeOn } from "../lib/serverStatus";
import { enqueue, playRadio, playTracks } from "../lib/player";
import { useStore } from "../lib/store";
import { CloseIcon, MatchIcon, PlayIcon, QueueIcon, ShuffleIcon } from "../components/Icons";
import { isLibrarySource, Sheet } from "../components/Sheets";
import { loadLibraryIds } from "../lib/library";
import { Cover, TrackRow } from "../components/TrackRow";

const PROVIDERS: Record<string, string> = {
  local: "Added here",
  ytmusic: "YouTube Music",
  lastfm: "Last.fm",
  spotify: "Spotify",
  deezer: "Deezer",
  demus: "Demus",
  file: "File",
  rematch: "Retry unmatched",
  "lastfm-sync": "Last.fm two-way sync",
};

function JobRow({ job }: { job: ImportJob }) {
  const pct = job.total ? Math.round((job.done / job.total) * 100) : 0;
  return (
    <div className="job">
      <div className="row between">
        <strong>{PROVIDERS[job.provider] ?? job.provider}</strong>
        <span className={`muted small status-${job.status}`}>
          {job.status === "running" ? `${job.done}/${job.total || "?"}` : job.status} · {job.matched} matched
        </span>
      </div>
      {job.status === "running" && (
        <div className="progress">
          <div style={{ width: `${pct}%` }} />
        </div>
      )}
      {job.note && <div className="muted small">{job.note}</div>}
      {job.error && <div className="error small">{job.error}</div>}
    </div>
  );
}

function ImportPanel({ status, onStarted }: { status: Status | null; onStarted: () => void }) {
  const toast = useStore((s) => s.toastMsg);
  const [deezerId, setDeezerId] = useState("");
  const [deezerToken, setDeezerToken] = useState("");
  const [text, setText] = useState("");
  // A file picked for "Other files" (used instead of the text box while set).
  const [file, setFile] = useState<File | null>(null);
  const ready = !!file || !!text.trim();
  const youtube = youtubeOn(status);

  useEffect(() => {
    if (status?.deezer.userId) setDeezerId(status.deezer.userId);
  }, [status]);

  const run = async (p: Promise<unknown>, label: string) => {
    try {
      await p;
      toast(`${label} import started`);
      onStarted();
      return true;
    } catch (e) {
      toast((e as Error).message);
      return false;
    }
  };

  const importOther = (lazy: boolean) => {
    const label = `${file ? file.name : "Pasted list"}${lazy ? "" : " (complete)"}`;
    const p = file ? api.library.importFile(file, "file", { lazy }) : api.library.importText(text, { lazy });
    return run(p, label).then((ok) => {
      if (!ok) return;
      setFile(null);
      setText("");
    });
  };

  return (
    <div className="panel">
      <h2>Import liked tracks</h2>
      <p className="muted small">
        {youtube
          ? "Tracks from other services are matched to YouTube Music automatically. Copies in your libraries (Plex, Jellyfin, music folder) are used for playback when available."
          : "YouTube is off, so imported songs are matched to your libraries (Plex, Jellyfin, music folder) when they play."}
      </p>
      {status && youtube && (
        <label className="field">
          <span className="small muted">
            Matching speed — slower means fewer refusals from YouTube (it pauses and retries when they happen)
          </span>
          <select
            className="input"
            value={status.prefs.importPace}
            onChange={(e) =>
              api.setPrefs({ ...status.prefs, importPace: e.target.value as typeof status.prefs.importPace }).then(onStarted)
            }
          >
            <option value="original">Original · no spacing, no retries (fastest; refused songs are marked to retry)</option>
            <option value="normal">Normal · about 4 s per song (~3 h for 2,700)</option>
            <option value="slow">Slow · about 8 s per song (~6 h for 2,700)</option>
            <option value="very_slow">Very slow · about 16 s per song (~12 h for 2,700)</option>
          </select>
        </label>
      )}

      {youtube && (
        <div className="import-row">
          <div>
            <strong>YouTube Music</strong>
            <div className="muted small">{status?.ytmusic.authenticated ? "Cookie configured" : "Needs cookie in Settings"}</div>
          </div>
          <button className="btn" disabled={!status?.ytmusic.authenticated} onClick={() => run(api.library.importYtMusic(), "YouTube Music")}>
            Import
          </button>
        </div>
      )}

      <div className="import-row column">
        <div className="row between">
          <div>
            <strong>Last.fm loved tracks</strong>
            <div className="muted small">
              {status?.lastfm.connected ? `Connected as ${status.lastfm.username}` : "Connect Last.fm in Settings first"}
            </div>
          </div>
          <div className="row gap">
            <button
              className="btn ghost"
              disabled={!status?.lastfm.connected}
              onClick={() => run(api.library.importLastfm(), "Last.fm")}
              title="Add loved tracks to the library (one way)"
            >
              Import
            </button>
            <button
              className="btn"
              disabled={!status?.lastfm.connected}
              onClick={() =>
                confirm(
                  "Two-way sync: every library song not yet loved on Last.fm gets loved, and loved tracks not in the library are added.\n\nAfter this first sync, removing a song on one side removes it on the other.",
                ) && run(api.library.syncLastfm(), "Last.fm sync")
              }
            >
              Sync both ways
            </button>
          </div>
        </div>
        {status?.lastfm.connected && (
          <label className="field">
            <span className="small muted">Every day</span>
            <select
              className="input"
              value={status.prefs.lastfmSync}
              onChange={(e) =>
                api.setPrefs({ ...status.prefs, lastfmSync: e.target.value as typeof status.prefs.lastfmSync }).then(onStarted)
              }
            >
              <option value="off">Don't sync</option>
              <option value="pull">Add new loves to the library (un-loves removed)</option>
              <option value="both">Two-way: library ↔ loved tracks</option>
            </select>
          </label>
        )}
      </div>

      {status?.prefs.deezer && (
        <div className="import-row column">
          <div>
            <strong>Deezer</strong>
            <div className="muted small">Profile ID or URL (profile must be public, or supply an access token)</div>
          </div>
          <div className="row gap wrap">
            <input className="input grow" placeholder="e.g. 123456789" value={deezerId} onChange={(e) => setDeezerId(e.target.value)} />
            <input className="input grow" placeholder="access token (optional)" value={deezerToken} onChange={(e) => setDeezerToken(e.target.value)} />
            <button className="btn" disabled={!deezerId.trim()} onClick={() => run(api.library.importDeezer(deezerId, deezerToken || undefined), "Deezer")}>
              Import
            </button>
          </div>
        </div>
      )}

      <div className="import-row column">
        <div>
          <strong>Other files / paste</strong>
          <div className="muted small">
            CSV (Exportify, TuneMyMusic, Soundiiz…), JSON, M3U/M3U8, plist, zip backups, YouTube links — or paste “Artist -
            Title” lines. For a Spotify playlist, export it as CSV from{" "}
            <a href="https://exportify.net/" target="_blank" rel="noreferrer">
              Exportify
            </a>{" "}
            or{" "}
            <a href="https://www.chosic.com/spotify-playlist-exporter/" target="_blank" rel="noreferrer">
              Chosic's playlist exporter
            </a>
            .
          </div>
        </div>
        {file ? (
          <div className="row gap">
            <span className="grow small">
              <strong>{file.name}</strong> <span className="muted">· {Math.max(1, Math.round(file.size / 1024))} KB</span>
            </span>
            <button className="icon-btn" aria-label="Clear file" title="Use the text box instead" onClick={() => setFile(null)}>
              <CloseIcon size={18} />
            </button>
          </div>
        ) : (
          <textarea className="input" rows={4} placeholder="Daft Punk - Get Lucky" value={text} onChange={(e) => setText(e.target.value)} />
        )}
        <div className="row gap wrap">
          <label className="btn ghost">
            Choose file…
            <input
              type="file"
              hidden
              onChange={(e) => {
                const f = e.target.files?.[0];
                e.target.value = "";
                if (f) setFile(f);
              }}
            />
          </label>
          <button
            className="btn"
            disabled={!ready}
            title={youtube ? "Adds the songs right away; each is found on YouTube the first time it plays" : "Adds the songs right away"}
            onClick={() => importOther(true)}
          >
            Import
          </button>
          {youtube && (
            <button
              className="btn ghost"
              disabled={!ready}
              title="Looks up every song on YouTube now (at the matching speed above), so unmatched ones show straight away"
              onClick={() => importOther(false)}
            >
              Complete import
            </button>
          )}
        </div>
        {youtube && (
          <div className="muted small">
            Import adds the songs straight away, and each is matched on YouTube the first time it plays. Complete import
            looks every song up now instead.
          </div>
        )}
      </div>
    </div>
  );
}

const MATCH_TABS: Record<SourceId, string> = { youtube: "YouTube", plex: "Plex", jellyfin: "Jellyfin", local: "Folder" };

/**
 * Pick what a library entry plays: a YouTube track, or a track from Plex, Jellyfin or the music
 * folder. A song that already has a match keeps it (for radio and as a fallback) and plays the
 * chosen library file; one without becomes a song of that library only.
 */
function MatchSheet({
  entry,
  status,
  onClose,
  onDone,
}: {
  entry: LikedEntry;
  status: Status | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const toast = useStore((s) => s.toastMsg);
  const initial = `${entry.artist} ${entry.title}`;
  // YouTube (while it's on) and each library that's set up, in priority order.
  const order = status?.prefs.sources?.map((p) => p.id) ?? (["plex", "jellyfin", "local", "youtube"] as SourceId[]);
  const tabs = order.filter((id) => (id === "youtube" ? youtubeOn(status) : sourceConnected(status, id)));
  const [picked, setSource] = useState<SourceId | null>(null);
  const source: SourceId | undefined = picked && tabs.includes(picked) ? picked : tabs.includes("youtube") ? "youtube" : tabs[0];
  const [q, setQ] = useState(initial);
  const [results, setResults] = useState<Track[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A match to keep when a library track is chosen (a YouTube song, or one matched when it plays).
  const keep = entry.track && !isLibraryOnlyId(entry.track.videoId) ? entry.track : null;

  const search = useCallback(async (from: SourceId | undefined, query: string) => {
    if (!from) return;
    setResults(null);
    setError(null);
    try {
      setResults(from === "youtube" ? await api.search(query) : await libraryApi(from).search(query));
    } catch (e) {
      setError((e as Error).message);
    }
  }, []);
  // Search again when the source changes, with whatever is in the box (typing searches on submit).
  useEffect(() => {
    search(source, q);
  }, [entry, source, search]); // eslint-disable-line react-hooks/exhaustive-deps

  const switchTo = (s: SourceId) => {
    // Library searches match titles best; YouTube's does better with the artist too.
    if (s !== "youtube" && q === initial) setQ(entry.title);
    if (s === "youtube" && q === entry.title) setQ(initial);
    setSource(s);
  };

  const use = async (t: Track) => {
    const item = libraryItem(t.videoId);
    try {
      if (item && keep) {
        await libraryApi(item.source).setMatch(keep.videoId, item.itemId);
        toast(`“${entry.title}” will play from ${SOURCE_LABELS[item.source]}`);
        onDone();
        return;
      }
      await api.library.match(entry.id, t);
      toast(item ? `Matched to “${t.title}” in ${SOURCE_LABELS[item.source]}` : `Matched to “${t.title}”`);
      loadLibraryIds();
      onDone();
    } catch (e) {
      toast((e as Error).message);
    }
  };

  const isCurrent = (t: Track) => entry.track?.videoId === t.videoId;
  const libName = isLibrarySource(source) ? SOURCE_LABELS[source] : "";

  return (
    <Sheet onClose={onClose}>
      <h3 className="sheet-title">{entry.track ? "Change match" : "Find match"}</h3>
      <p className="small muted" style={{ margin: "0 8px 10px" }}>
        For <strong>{entry.title}</strong> by {entry.artist}
        {entry.album ? ` · ${entry.album}` : ""} ({PROVIDERS[entry.provider] ?? entry.provider})
      </p>
      {tabs.length > 1 && (
        <div className="tabs" role="tablist" style={{ margin: "0 8px 10px" }}>
          {tabs.map((s) => (
            <button key={s} role="tab" aria-selected={source === s} className={source === s ? "on" : ""} onClick={() => switchTo(s)}>
              {MATCH_TABS[s]}
            </button>
          ))}
        </div>
      )}
      {!source && (
        <p className="small muted" style={{ margin: "0 8px 10px" }}>
          Turn on YouTube or connect a library (Plex, Jellyfin or a music folder) in Settings → Audio sources to find a match.
        </p>
      )}
      {libName && (
        <p className="small muted" style={{ margin: "0 8px 10px" }}>
          {keep
            ? `Plays the ${libName} file you pick, in the order set in Settings → Audio sources. The current match stays for radio and as a fallback.`
            : `Adds the song from ${libName}. It only plays while ${libName} is reachable.`}
        </p>
      )}
      {source && (
        <form
          className="row gap"
          onSubmit={(e) => {
            e.preventDefault();
            search(source, q);
          }}
        >
          <input className="input" value={q} onChange={(e) => setQ(e.target.value)} aria-label={`Search ${MATCH_TABS[source]}`} />
          <button className="btn" type="submit">
            Search
          </button>
        </form>
      )}
      {error && <p className="error small">{error}</p>}
      {source && !results && !error && <div className="spinner" />}
      <div className="sheet-list">
        {results?.map((t) => (
          <div key={t.videoId} className={`match-row ${isCurrent(t) ? "current" : ""}`}>
            <button
              className="track-main"
              onClick={() => (isLibraryOnlyId(t.videoId) ? playTracks([t]) : playRadio(t))}
              title="Preview"
            >
              <Cover src={t.thumbnail} size={44} videoId={t.videoId} />
              <div className="track-text">
                <div className="track-title">{t.title}</div>
                <div className="track-sub">
                  {t.artist}
                  {t.album ? ` · ${t.album}` : ""} · {fmtTime(t.duration)}
                </div>
              </div>
            </button>
            {isCurrent(t) ? (
              <span className="small muted">current</span>
            ) : (
              <button className="btn small" onClick={() => use(t)}>
                Use this
              </button>
            )}
          </div>
        ))}
        {results?.length === 0 && (
          <p className="muted small">
            {libName ? `Nothing in ${libName} for that — try a shorter search (just the title).` : "No results — try a different search."}
          </p>
        )}
      </div>
    </Sheet>
  );
}

export default function Library() {
  const [params] = useSearchParams();
  const tick = useStore((s) => s.importTick);
  const toast = useStore((s) => s.toastMsg);
  const [liked, setLikedEntries] = useState<LikedEntry[]>([]);
  const [jobs, setJobs] = useState<ImportJob[]>([]);
  const [status, setStatus] = useState<Status | null>(null);
  const [provider, setProvider] = useState("");
  const [onlyUnmatched, setOnlyUnmatched] = useState(false);
  const [q, setQ] = useState("");
  const [showImport, setShowImport] = useState(false);
  const [matching, setMatching] = useState<LikedEntry | null>(null);

  const load = useCallback(() => {
    api.library
      .liked({ provider, q, status: onlyUnmatched ? "unmatched" : undefined })
      .then(setLikedEntries)
      .catch(() => {});
    api.library.jobs().then(setJobs).catch(() => {});
  }, [provider, q, onlyUnmatched]);

  const loadStatus = useCallback(() => fetchStatus().then(setStatus).catch(() => {}), []);

  useEffect(() => {
    const t = setTimeout(load, 200);
    return () => clearTimeout(t);
  }, [load, tick]);

  useEffect(() => {
    loadStatus();
    if (params.get("import")) setShowImport(true);
  }, [params, loadStatus]);

  const running = jobs.some((j) => j.status === "running");
  useEffect(() => {
    if (!running) return;
    const t = setInterval(load, 3000);
    return () => clearInterval(t);
  }, [running, load]);
  useEffect(() => {
    if (!running) loadLibraryIds();
  }, [running]);

  const playable: Track[] = liked.filter((l) => l.track).map((l) => l.track!);
  const unmatched = liked.filter((l) => l.status === "unmatched" || l.status === "error").length;

  const remove = async (l: LikedEntry) => {
    await api.library.remove(l.id);
    setLikedEntries((cur) => cur.filter((x) => x.id !== l.id));
    loadLibraryIds();
    toast(`Removed “${l.title}” from your library`);
  };

  return (
    <div className="page">
      <div className="row between">
        <h1>Library</h1>
        <button className="btn ghost small" onClick={() => setShowImport((s) => !s)}>
          {showImport ? "Hide import" : "Import…"}
        </button>
      </div>

      {(showImport || liked.length === 0) && (
        <ImportPanel
          status={status}
          onStarted={() => {
            load();
            loadStatus();
          }}
        />
      )}
      {jobs.filter((j) => j.status === "running" || Date.now() / 1000 - (j.finishedAt ?? 0) < 600).map((j) => (
        <JobRow key={j.id} job={j} />
      ))}

      <div className="row gap wrap">
        <input className="input grow" type="search" placeholder="Filter liked tracks" value={q} onChange={(e) => setQ(e.target.value)} />
        <select className="input" value={provider} onChange={(e) => setProvider(e.target.value)}>
          <option value="">All sources</option>
          {Object.entries(PROVIDERS)
            .filter(([k]) => k !== "rematch")
            .map(([k, v]) => (
              <option key={k} value={k}>
                {v}
              </option>
            ))}
        </select>
        <label className="switch">
          <input type="checkbox" checked={onlyUnmatched} onChange={(e) => setOnlyUnmatched(e.target.checked)} />
          <span>Unmatched only</span>
        </label>
      </div>

      <div className="row gap wrap">
        {playable.length > 0 && (
          <>
            <button className="btn" onClick={() => playTracks(playable)}>
              <PlayIcon size={16} /> Play {playable.length}
            </button>
            <button className="btn ghost" onClick={() => playTracks([...playable].sort(() => Math.random() - 0.5))}>
              <ShuffleIcon size={16} /> Shuffle
            </button>
            <button className="btn ghost" onClick={() => enqueue(playable, "end")}>
              <QueueIcon size={16} /> Queue all
            </button>
          </>
        )}
        {unmatched > 0 && (
          <button
            className="btn ghost"
            disabled={running}
            title="Search again for tracks that didn't match (including ones whose lookup failed)"
            onClick={() =>
              api.library
                .rematch()
                .then(() => {
                  toast(`Retrying ${unmatched} unmatched tracks`);
                  load();
                })
                .catch((e) => toast(e.message))
            }
          >
            <MatchIcon size={16} /> Retry unmatched ({unmatched})
          </button>
        )}
      </div>

      <div className="track-list">
        {liked.map((l) =>
          l.track ? (
            <TrackRow
              key={l.id}
              track={l.track}
              onPlay={() => playTracks(playable, playable.indexOf(l.track!))}
              trailing={
                <>
                  <span
                    className="provider-tag"
                    title={
                      l.status === "manual"
                        ? "Matched by you"
                        : l.status === "deferred"
                          ? "Found on YouTube the first time it plays"
                          : undefined
                    }
                  >
                    {PROVIDERS[l.provider] ?? l.provider}
                    {l.status === "manual" ? " · ✓" : l.status === "deferred" ? " · on play" : ""}
                  </span>
                  <button className="icon-btn" aria-label="Change match" title="Change match" onClick={() => setMatching(l)}>
                    <MatchIcon size={18} />
                  </button>
                </>
              }
            />
          ) : (
            <div key={l.id} className="track-row unmatched">
              <div className="track-main">
                <div className="track-text">
                  <div className="track-title">{l.title}</div>
                  <div className="track-sub">
                    {l.artist} ·{" "}
                    <span className="error">{l.status === "error" ? "lookup failed — retry" : "no match found"}</span>
                  </div>
                </div>
              </div>
              <span className="provider-tag">{PROVIDERS[l.provider] ?? l.provider}</span>
              <button className="btn small ghost" onClick={() => setMatching(l)}>
                Find match
              </button>
              <button className="icon-btn" aria-label="Remove from library" title="Remove from library" onClick={() => remove(l)}>
                <CloseIcon size={18} />
              </button>
            </div>
          ),
        )}
      </div>

      {matching && (
        <MatchSheet
          entry={matching}
          status={status}
          onClose={() => setMatching(null)}
          onDone={() => {
            setMatching(null);
            load();
          }}
        />
      )}
    </div>
  );
}
