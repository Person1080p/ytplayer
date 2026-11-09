import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import {
  api,
  type LinkedPlaylist,
  type Playlist,
  type PlaylistSummary,
  type SpotifyPlaylist,
  type SpotifySummary,
} from "../lib/api";
import { enqueue, playTracks } from "../lib/player";
import { useStore } from "../lib/store";
import { useYoutubeOn } from "../lib/serverStatus";
import {
  BackIcon,
  CloseIcon,
  LinkIcon,
  ListIcon,
  PlayIcon,
  PlusIcon,
  QueueIcon,
  ShuffleIcon,
  TrashIcon,
} from "../components/Icons";
import { Cover, TrackList, TrackRow } from "../components/TrackRow";

export function PlaylistsPage() {
  const [lists, setLists] = useState<PlaylistSummary[] | null>(null);
  const [linked, setLinked] = useState<LinkedPlaylist[]>([]);
  const [name, setName] = useState("");
  const queue = useStore((s) => s.queue);
  const toast = useStore((s) => s.toastMsg);
  // Linked playlists open from YouTube Music, so they're hidden while YouTube is off.
  const youtube = useYoutubeOn();
  const load = () => {
    api.playlists.list().then(setLists).catch((e) => toast(e.message));
    api.playlists.linked().then(setLinked).catch(() => {});
  };
  const unlink = async (p: LinkedPlaylist) => {
    await api.playlists.unlink(p.id);
    setLinked((cur) => cur.filter((x) => x.id !== p.id));
    toast(`Removed “${p.title}” from Playlists`);
  };
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const create = async (fromQueue: boolean) => {
    const n = name.trim() || (fromQueue ? `Queue ${new Date().toLocaleDateString()}` : "");
    if (!n) return;
    await api.playlists.create(n, fromQueue ? (queue?.items ?? []).map((i) => i.track) : []);
    setName("");
    load();
  };

  return (
    <div className="page">
      <h1>Playlists</h1>
      <form
        className="row gap wrap"
        onSubmit={(e) => {
          e.preventDefault();
          create(false);
        }}
      >
        <input className="input grow" placeholder="New playlist" value={name} onChange={(e) => setName(e.target.value)} />
        <button className="btn" type="submit" disabled={!name.trim()}>
          <PlusIcon size={16} /> Create
        </button>
        <button className="btn ghost" type="button" onClick={() => create(true)} disabled={!queue?.items.length}>
          Save current queue
        </button>
      </form>
      {!lists && <div className="spinner" />}
      <div className="grid">
        {lists?.map((p) => (
          <Link key={p.id} to={`/playlists/${p.id}`} className="card-btn">
            <Cover src={p.thumbnail} size={148} />
            <div className="card-title">{p.name}</div>
            <div className="card-sub">{p.count} tracks</div>
          </Link>
        ))}
      </div>
      {lists?.length === 0 && <p className="muted">No playlists yet. Use ⋯ → “Add to playlist” on any track.</p>}

      {!youtube && linked.length > 0 && (
        <p className="small muted">
          Your {linked.length} linked YouTube Music playlist{linked.length === 1 ? " is" : "s are"} hidden while YouTube is off
          (Settings → Audio sources).
        </p>
      )}
      {youtube && linked.length > 0 && (
        <section className="shelf">
          <div className="shelf-head">
            <h2>From YouTube Music</h2>
          </div>
          <p className="small muted">Linked playlists and albums open from YouTube Music, so they stay up to date.</p>
          <div className="grid">
            {linked.map((p) => (
              <div key={p.id} className="linked-card">
                <Link to={`/collection/${encodeURIComponent(p.id)}?title=${encodeURIComponent(p.title)}`} className="card-btn">
                  <Cover src={p.thumbnail} size={148} />
                  <div className="card-title">{p.title}</div>
                  <div className="card-sub">{p.subtitle || (p.id.startsWith("MPRE") ? "Album" : "Playlist")}</div>
                </Link>
                <button className="icon-btn linked-remove" aria-label={`Remove ${p.title} from Playlists`} title="Remove from Playlists" onClick={() => unlink(p)}>
                  <CloseIcon size={16} />
                </button>
              </div>
            ))}
          </div>
        </section>
      )}
      {lists && linked.length === 0 && youtube && (
        <p className="small muted">Open a playlist or album from Home or Search and use “Link in Playlists” to keep it here.</p>
      )}

      <SpotifySection />
    </div>
  );
}

/** Chosic's Spotify playlist exporter: takes any playlist's link and downloads it as CSV. */
const CHOSIC_EXPORTER = "https://www.chosic.com/spotify-playlist-exporter/";

/**
 * Copy text inside a click. The Clipboard API only works on https:// (or localhost) pages, so
 * this uses the older copy command, which also works on a plain http:// address.
 */
function copyText(text: string): boolean {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  ta.remove();
  if (!ok) navigator.clipboard?.writeText(text).catch(() => {});
  return ok || !!navigator.clipboard;
}

function ago(at: number) {
  const s = Math.max(0, Date.now() / 1000 - at);
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400 * 2) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
}

/** Spotify playlists imported from a link, and the import box. */
function SpotifySection() {
  const nav = useNavigate();
  const toast = useStore((s) => s.toastMsg);
  const [lists, setLists] = useState<SpotifySummary[]>([]);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    api.playlists.spotify().then(setLists).catch(() => {});
  }, []);

  const importIt = async () => {
    if (!url.trim()) return;
    setBusy(true);
    try {
      const p = await api.playlists.spotifyImport(url.trim());
      setUrl("");
      toast(`Imported “${p.name}” (${p.tracks.length} songs)`);
      nav(`/spotify/${p.id}`);
    } catch (e) {
      toast((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const remove = async (p: SpotifySummary) => {
    await api.playlists.spotifyRemove(p.id);
    setLists((cur) => cur.filter((x) => x.id !== p.id));
    toast(`Removed “${p.name}”`);
  };

  return (
    <section className="shelf">
      <div className="shelf-head">
        <h2>From Spotify</h2>
      </div>
      <form
        className="row gap wrap"
        onSubmit={(e) => {
          e.preventDefault();
          importIt();
        }}
      >
        <input
          className="input grow"
          placeholder="Paste a Spotify playlist link"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          aria-label="Spotify playlist link"
        />
        <button className="btn" type="submit" disabled={busy || !url.trim()}>
          {busy ? "Importing…" : "Import"}
        </button>
      </form>
      <p className="small muted">
        The list stays in sync with Spotify. Songs are found on YouTube (or in Plex) only when they play. Spotify shows
        the first 100 songs; for longer playlists, “Sync now” on the playlist gets the rest through Chosic.
      </p>
      {lists.length > 0 && (
        <div className="grid">
          {lists.map((p) => (
            <div key={p.id} className="linked-card">
              <Link to={`/spotify/${p.id}`} className="card-btn">
                <Cover src={p.thumbnail} size={148} />
                <div className="card-title">{p.name}</div>
                <div className="card-sub">
                  {p.count} songs{p.owner ? ` · ${p.owner}` : ""}
                </div>
              </Link>
              <button className="icon-btn linked-remove" aria-label={`Remove ${p.name}`} title="Remove" onClick={() => remove(p)}>
                <CloseIcon size={16} />
              </button>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

/** A Spotify playlist: the list from Spotify (resynced when opened after a while). */
export function SpotifyPlaylistPage() {
  const { id = "" } = useParams();
  const nav = useNavigate();
  const pick = useStore((s) => s.pickPlaylist);
  const [pl, setPl] = useState<SpotifyPlaylist | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  // Spotify's public page lists 100 songs at most; past that, the list comes from a playlist
  // export: Sync now copies the playlist's link and opens Chosic's exporter (it takes any
  // playlist's link; paste it there). The server picks the downloaded CSV up from the Downloads
  // folder by itself (or it can be dropped here).
  const [exportOpen, setExportOpen] = useState(false);
  const [exportStartedAt, setExportStartedAt] = useState(0);
  const [copied, setCopied] = useState(false);
  const [uploading, setUploading] = useState(false);
  const toast = useStore((s) => s.toastMsg);
  const needsExport = !!pl && (pl.tracks.length >= 100 || pl.source === "export");
  const link = `https://open.spotify.com/playlist/${id}`;

  const syncNow = () => {
    // Both right in the click: the copy and the new tab need it.
    if (needsExport) {
      setCopied(copyText(link));
      window.open(CHOSIC_EXPORTER, "_blank", "noopener");
      setExportOpen(true);
      setExportStartedAt(Math.floor(Date.now() / 1000));
    }
    load(true);
  };

  // While waiting for the export: check every few seconds whether the server has loaded it (a
  // stored playlist that's fresh is just read from the database, nothing is fetched).
  useEffect(() => {
    if (!exportOpen || !exportStartedAt) return;
    const timer = setInterval(() => {
      api.playlists
        .spotifyGet(id)
        .then((p) => {
          if ((p.exportAt ?? 0) < exportStartedAt) return;
          setPl(p);
          setExportOpen(false);
          toast(`Loaded all ${p.tracks.length} songs from the export`);
        })
        .catch(() => {});
    }, 3000);
    // Give up after 10 minutes (the panel stays, for dropping the file).
    const stop = setTimeout(() => clearInterval(timer), 600_000);
    return () => {
      clearInterval(timer);
      clearTimeout(stop);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [exportOpen, exportStartedAt, id]);
  const loadExport = async (file: File | undefined | null) => {
    if (!file) return;
    if (!/\.csv$/i.test(file.name) && file.type !== "text/csv") {
      toast("That isn't a CSV file (export the playlist as CSV)");
      return;
    }
    setUploading(true);
    try {
      const p = await api.playlists.spotifyExport(id, await file.text());
      setPl(p);
      setExportOpen(false);
      toast(`Loaded all ${p.tracks.length} songs from the export`);
    } catch (e) {
      toast((e as Error).message);
    } finally {
      setUploading(false);
    }
  };

  const load = (refresh = false) => {
    if (refresh) setSyncing(true);
    api.playlists
      .spotifyGet(id, refresh)
      .then((p) => {
        setPl(p);
        setError(null);
      })
      .catch((e) => setError((e as Error).message))
      .finally(() => setSyncing(false));
  };
  useEffect(() => {
    setPl(null);
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  // A song shows the playlist's cover until it's first played; then the server looks up its own
  // art and puts it in the queue. Take it from there, so rows update while the page is open.
  const queueItems = useStore((s) => s.queue?.items);
  const tracks = useMemo(() => {
    if (!pl) return [];
    const art = new Map<string, string>();
    for (const it of queueItems ?? []) {
      const th = it.track.thumbnail;
      if (th && th !== pl.thumbnail && it.track.videoId.startsWith("spotify_")) art.set(it.track.videoId, th);
    }
    if (!art.size) return pl.tracks;
    return pl.tracks.map((x) => {
      const th = art.get(x.videoId);
      return th && th !== x.thumbnail ? { ...x, thumbnail: th } : x;
    });
  }, [pl, queueItems]);

  if (error && !pl)
    return (
      <div className="page">
        <button className="btn ghost small" onClick={() => nav("/playlists")}>
          <BackIcon size={16} /> Playlists
        </button>
        <p className="error">{error}</p>
      </div>
    );
  if (!pl) return <div className="page"><div className="spinner" /></div>;
  const t = tracks;

  return (
    <div
      className="page"
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes("Files")) e.preventDefault();
      }}
      onDrop={(e) => {
        if (!e.dataTransfer.files.length) return;
        e.preventDefault();
        loadExport(e.dataTransfer.files[0]);
      }}
    >
      <button className="btn ghost small" onClick={() => nav("/playlists")}>
        <BackIcon size={16} /> Playlists
      </button>
      <div className="collection-head">
        <Cover src={pl.thumbnail} size={120} />
        <div>
          <h1>{pl.name}</h1>
          <p className="muted">
            Spotify{pl.owner ? ` · ${pl.owner}` : ""} · {t.length} songs · synced {ago(pl.syncedAt)}
          </p>
        </div>
      </div>
      {pl.note && <p className="notice small">{pl.note}</p>}
      <div className="row gap wrap">
        <button className="btn" onClick={() => playTracks(t)} disabled={!t.length}>
          <PlayIcon size={16} /> Play
        </button>
        <button className="btn ghost" onClick={() => playTracks([...t].sort(() => Math.random() - 0.5))} disabled={!t.length}>
          <ShuffleIcon size={16} /> Shuffle
        </button>
        <button className="btn ghost" onClick={() => enqueue(t, "end")} disabled={!t.length}>
          <QueueIcon size={16} /> Queue all
        </button>
        <button className="btn ghost" onClick={() => pick(t)} disabled={!t.length} title="Copy these songs into one of your playlists">
          <ListIcon size={16} /> Save a copy
        </button>
        <button
          className="btn ghost"
          disabled={syncing || uploading}
          onClick={syncNow}
          title={
            needsExport
              ? "Get the first 100 songs from Spotify again, and open Chosic (with the link copied) for the full list"
              : "Get the list from Spotify again now"
          }
        >
          {syncing ? "Syncing…" : uploading ? "Loading list…" : "Sync now"}
        </button>
        <a className="btn ghost" href={`https://open.spotify.com/playlist/${pl.id}`} target="_blank" rel="noreferrer">
          <LinkIcon size={16} /> Open in Spotify
        </a>
      </div>
      {exportOpen && (
        <div className="notice export-drop">
          <strong>Full list through Chosic</strong>
          <p className="small" style={{ margin: "6px 0 8px" }}>
            {copied ? "The playlist's link is copied. " : ""}In the{" "}
            <a href={CHOSIC_EXPORTER} target="_blank" rel="noreferrer">
              Chosic
            </a>{" "}
            tab, paste {copied ? "it (Ctrl+V / ⌘V)" : "the link below"} into the playlist box, load the playlist and
            download it as CSV. The download is picked up from your Downloads folder and the full list appears here by
            itself.
          </p>
          <div className="row gap wrap" style={{ marginBottom: 8 }}>
            <code className="small">{link}</code>
            <button className="btn ghost small" onClick={() => setCopied(copyText(link))}>
              {copied ? "Copy again" : "Copy link"}
            </button>
          </div>
          <div className="row gap wrap">
            <span className="muted small">Waiting for the export… </span>
            <label className={`btn ghost small ${uploading ? "disabled" : ""}`} title="If the download went somewhere else, or you're on another device">
              {uploading ? "Loading…" : "Or choose the CSV"}
              <input
                type="file"
                accept=".csv,text/csv"
                hidden
                disabled={uploading}
                onChange={(e) => {
                  loadExport(e.target.files?.[0]);
                  e.target.value = "";
                }}
              />
            </label>
            <button className="btn ghost small" onClick={() => setExportOpen(false)}>
              Not now
            </button>
          </div>
        </div>
      )}
      {error && <p className="error small">{error}</p>}
      <TrackList tracks={t} onPlay={(i) => playTracks(t, i)} />
      {!t.length && <p className="muted">This playlist is empty on Spotify.</p>}
    </div>
  );
}

export function PlaylistDetail() {
  const { id } = useParams();
  const nav = useNavigate();
  const toast = useStore((s) => s.toastMsg);
  const [pl, setPl] = useState<Playlist | null>(null);

  useEffect(() => {
    api.playlists.get(Number(id)).then(setPl).catch((e) => toast(e.message));
  }, [id, toast]);

  if (!pl) return <div className="page"><div className="spinner" /></div>;

  const save = async (tracks = pl.tracks, name?: string) => {
    setPl(await api.playlists.update(pl.id, { tracks, name }));
  };
  const move = (i: number, d: number) => {
    const j = i + d;
    if (j < 0 || j >= pl.tracks.length) return;
    const t = [...pl.tracks];
    [t[i], t[j]] = [t[j], t[i]];
    save(t);
  };

  return (
    <div className="page">
      <button className="btn ghost small" onClick={() => nav("/playlists")}>
        <BackIcon size={16} /> Playlists
      </button>
      <div className="collection-head">
        <Cover src={pl.tracks[0]?.thumbnail} size={120} />
        <div>
          <h1
            contentEditable
            suppressContentEditableWarning
            onBlur={(e) => {
              const n = e.currentTarget.textContent?.trim();
              if (n && n !== pl.name) save(undefined, n);
            }}
          >
            {pl.name}
          </h1>
          <p className="muted">{pl.tracks.length} tracks · tap the name to rename</p>
        </div>
      </div>
      <div className="row gap wrap">
        <button className="btn" onClick={() => playTracks(pl.tracks)} disabled={!pl.tracks.length}>
          <PlayIcon size={16} /> Play
        </button>
        <button
          className="btn ghost"
          onClick={() => playTracks([...pl.tracks].sort(() => Math.random() - 0.5))}
          disabled={!pl.tracks.length}
        >
          <ShuffleIcon size={16} /> Shuffle
        </button>
        <button className="btn ghost" onClick={() => enqueue(pl.tracks, "end")} disabled={!pl.tracks.length}>
          <QueueIcon size={16} /> Queue all
        </button>
        <button
          className="btn ghost danger"
          onClick={async () => {
            if (!confirm(`Delete “${pl.name}”?`)) return;
            await api.playlists.remove(pl.id);
            nav("/playlists");
          }}
        >
          <TrashIcon size={16} /> Delete
        </button>
      </div>
      <div className="track-list">
        {pl.tracks.map((t, i) => (
          <TrackRow
            key={`${t.videoId}-${i}`}
            track={t}
            onPlay={() => playTracks(pl.tracks, i)}
            leading={
              <span className="reorder">
                <button className="icon-btn tiny" onClick={() => move(i, -1)} aria-label="Move up">▲</button>
                <button className="icon-btn tiny" onClick={() => move(i, 1)} aria-label="Move down">▼</button>
              </span>
            }
            trailing={
              <button className="icon-btn" aria-label="Remove" onClick={() => save(pl.tracks.filter((_, j) => j !== i))}>
                <CloseIcon size={18} />
              </button>
            }
          />
        ))}
      </div>
    </div>
  );
}
