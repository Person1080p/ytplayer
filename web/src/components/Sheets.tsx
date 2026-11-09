import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  api,
  fmtTime,
  isDeferredId,
  isLibraryOnlyId,
  libraryApi,
  libraryItem,
  SOURCE_LABELS,
  type LibraryChoice,
  type LibrarySourceId,
  type PlaylistSummary,
  type SourceId,
  type SourceInfo,
  type Status,
  type Track,
  type YoutubeMatch,
} from "../lib/api";
import { setLiked, useLibrary } from "../lib/library";
import { enqueue, playRadio, reloadSource } from "../lib/player";
import { sourceConnected, sourceOn, useStatus, youtubeOn } from "../lib/serverStatus";
import { localUrl, prepareLocal } from "../lib/localFolder";
import { currentTrack, useStore } from "../lib/store";
import {
  HeartFilledIcon,
  HeartIcon,
  ListIcon,
  MatchIcon,
  PlayIcon,
  PlayNextIcon,
  PlusIcon,
  QueueIcon,
  RadioIcon,
  SearchIcon,
} from "./Icons";
import { Cover } from "./TrackRow";
import { artistLink } from "./Shelf";

/** Where a song plays from now: the music folder on this device, else what the server says. */
async function sourceNow(track: Track): Promise<SourceInfo> {
  await prepareLocal(track);
  if (localUrl(track.videoId)) {
    return { source: "local", kbps: null, item: { source: "local", itemId: "", title: track.title, artist: track.artist } };
  }
  return api.source(track.videoId);
}

export function Sheet({ onClose, children }: { onClose: () => void; children: React.ReactNode }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-grip" />
        {children}
      </div>
    </div>
  );
}

/** Actions for a single track: play now/next, queue, radio, playlist, love. */
export function TrackMenu() {
  const track = useStore((s) => s.menuTrack);
  const close = () => useStore.getState().openMenu(null);
  const pick = useStore((s) => s.pickPlaylist);
  const nav = useNavigate();
  const inLibrary = useLibrary((s) => (track ? s.ids.has(track.videoId) : false));
  const status = useStatus();
  if (!track) return null;

  const act = (fn: () => void) => () => {
    fn();
    close();
  };

  return (
    <Sheet onClose={close}>
      <div className="sheet-head">
        <Cover src={track.thumbnail} size={52} videoId={track.videoId} />
        <div className="track-text">
          <div className="track-title">{track.title}</div>
          <div className="track-sub">{track.artist}</div>
        </div>
      </div>
      <button className="sheet-item" onClick={act(() => enqueue([track], "now"))}>
        <PlayIcon size={20} /> Play now
      </button>
      <button className="sheet-item" onClick={act(() => enqueue([track], "next"))}>
        <PlayNextIcon size={20} /> Play next
      </button>
      <button className="sheet-item" onClick={act(() => enqueue([track], "end"))}>
        <QueueIcon size={20} /> Add to queue
      </button>
      <button className="sheet-item" onClick={act(() => playRadio(track))}>
        <RadioIcon size={20} /> Start radio
      </button>
      <button className="sheet-item" onClick={() => pick([track])}>
        <ListIcon size={20} /> Add to playlist…
      </button>
      <button className="sheet-item" onClick={act(() => setLiked(track, !inLibrary))}>
        {inLibrary ? <HeartFilledIcon size={20} /> : <HeartIcon size={20} />}
        {inLibrary ? "Remove from library" : "Add to library"}
      </button>
      {/* Artist pages come from YouTube Music. */}
      {youtubeOn(status) && (
        <button
          className="sheet-item"
          onClick={act(() => {
            useStore.getState().setNowPlaying(false);
            nav(artistLink(track.artist));
          })}
        >
          <SearchIcon size={20} /> Go to {track.artist.split(",")[0]}
        </button>
      )}
      {sourceTabs(track, status).length > 0 && (
        <button className="sheet-item" onClick={() => useStore.getState().choosePlexVersion(track)}>
          <MatchIcon size={20} /> Change audio source…
        </button>
      )}
    </Sheet>
  );
}

const TAB_LABELS: Record<SourceId, string> = { youtube: "YouTube", plex: "Plex", jellyfin: "Jellyfin", local: "Folder" };
const DEFAULT_ORDER: SourceId[] = ["plex", "jellyfin", "local", "youtube"];

/**
 * The audio source sheet's tabs for a song, in priority order: YouTube (while it's on) and each
 * library that's set up. A song that's only in one library gets the other libraries.
 */
export function sourceTabs(track: Track, status: Status | null): SourceId[] {
  const own = libraryItem(track.videoId)?.source;
  const order = status?.prefs.sources?.map((p) => p.id) ?? DEFAULT_ORDER;
  return order.filter((id) =>
    id === "youtube" ? !isLibraryOnlyId(track.videoId) && youtubeOn(status) : id !== own && sourceConnected(status, id),
  );
}

export const isLibrarySource = (s: string | null | undefined): s is LibrarySourceId =>
  s === "plex" || s === "jellyfin" || s === "local";

/**
 * Change where a song's audio comes from. A tab per source:
 * - YouTube: the video it plays from (a song's own video, or one matched for a Spotify or imported
 *   song), a search of YouTube's songs and videos to pick another, or "look again".
 * - Plex, Jellyfin, Folder: automatic matching, a copy you pick (the studio recording rather than
 *   a live one), or never that library for this song.
 * Changes apply straight away; the song switches over in place if it's playing.
 */
export function AudioSourceSheet() {
  const track = useStore((s) => s.plexMatchTrack);
  const status = useStatus();
  const missing = useStore((s) => (track ? s.missing.has(track.videoId) : false));
  const close = () => useStore.getState().choosePlexVersion(null);
  // A tab picked by hand; otherwise the sheet opens on the source that plays now.
  const [picked, setPicked] = useState<SourceId | null>(null);
  const [now, setNow] = useState<SourceInfo | "error" | null>(null);
  const [yt, setYt] = useState<YoutubeMatch | null>(null);
  const [choices, setChoices] = useState<Partial<Record<LibrarySourceId, LibraryChoice | Error>>>({});

  const tabs = track ? sourceTabs(track, status) : [];
  const hasYoutube = tabs.includes("youtube");
  const libraryKey = tabs.filter(isLibrarySource).join(",");

  useEffect(() => {
    setPicked(null);
    setNow(null);
    setYt(null);
    setChoices({});
    if (track) sourceNow(track).then(setNow, () => setNow("error"));
  }, [track]);
  useEffect(() => {
    if (track && hasYoutube) api.youtubeMatch(track.videoId).then(setYt, () => {});
  }, [track, hasYoutube]);
  useEffect(() => {
    if (!track || !libraryKey) return;
    let live = true;
    for (const id of libraryKey.split(",") as LibrarySourceId[]) {
      libraryApi(id)
        .match(track.videoId)
        .then(
          (c): LibraryChoice | Error => c,
          (e) => (e instanceof Error ? e : new Error(String(e))),
        )
        .then((c) => live && setChoices((cur) => ({ ...cur, [id]: c })));
    }
    return () => {
      live = false;
    };
  }, [track, libraryKey]);
  if (!track) return null;

  const info = now && now !== "error" ? now : null;
  const playsFrom = info?.source ?? null;
  const libraryPlays = isLibrarySource(playsFrom) ? playsFrom : null;
  const autoTab: SourceId | undefined =
    (libraryPlays && tabs.includes(libraryPlays) ? libraryPlays : undefined) ?? (hasYoutube ? "youtube" : tabs[0]);
  const tab = picked && tabs.includes(picked) ? picked : autoTab;
  const deferred = isDeferredId(track.videoId);

  const youtubeLine = () =>
    yt?.kind === "picked"
      ? `Plays from a YouTube video you picked${yt.track ? `: ${yt.track.title} · ${yt.track.artist}` : ""}.`
      : !deferred
        ? "Plays from YouTube (its own video)."
        : yt?.kind === "matched" && yt.track
          ? `Plays from YouTube: ${yt.track.title} · ${yt.track.artist}.`
          : yt?.kind === "none"
            ? "Nothing was found on YouTube. Pick a video below."
            : yt
              ? "Found on YouTube the first time it plays, or pick one below."
              : "…";
  const line =
    missing && !libraryPlays
      ? "No audio found for this song. Pick a source below."
      : libraryPlays
        ? `Plays from ${SOURCE_LABELS[libraryPlays]}${info?.item ? `: ${info.item.title} · ${info.item.artist}` : ""}.`
        : playsFrom === "cache"
          ? "Plays from the server's MP3 copy of a YouTube play."
          : playsFrom === "youtube" || (now === "error" && hasYoutube)
            ? youtubeLine()
            : now === "error"
              ? ""
              : "…";

  return (
    <Sheet onClose={close}>
      <div className="sheet-head">
        <Cover src={track.thumbnail} size={52} videoId={track.videoId} />
        <div className="track-text">
          <div className="track-title">Change audio source</div>
          <div className="track-sub">
            {track.title} · {track.artist} · {fmtTime(track.duration)}
          </div>
        </div>
      </div>
      {line && <p className={`small source-now ${missing && !libraryPlays ? "missing" : ""}`}>{line}</p>}
      {tabs.length > 1 && (
        <div className="tabs" role="tablist" style={{ margin: "0 8px 10px" }}>
          {tabs.map((t) => (
            <button key={t} role="tab" aria-selected={tab === t} className={tab === t ? "on" : ""} onClick={() => setPicked(t)}>
              {TAB_LABELS[t]}
            </button>
          ))}
        </div>
      )}
      {tab === "youtube" ? (
        <YoutubePane track={track} match={yt} onMatch={setYt} libraryPlays={libraryPlays} missing={missing} onDone={close} />
      ) : tab ? (
        <LibraryPane key={tab} source={tab} track={track} choice={choices[tab] ?? null} on={sourceOn(status, tab)} onDone={close} />
      ) : (
        <p className="muted small" style={{ margin: "0 8px" }}>
          Turn on YouTube or connect a library (Plex, Jellyfin or a music folder) in Settings → Audio sources to choose where
          this song plays from.
        </p>
      )}
    </Sheet>
  );
}

/** Apply a change, switch a playing song over in place, and close. */
function useApply(track: Track, onDone: () => void) {
  const playingId = useStore((s) => currentTrack(s)?.videoId);
  const toast = useStore((s) => s.toastMsg);
  const [busy, setBusy] = useState(false);
  const run = async (fn: () => Promise<unknown>, msg: string) => {
    setBusy(true);
    try {
      await fn();
      reloadSource(track.videoId);
      toast(msg + (track.videoId === playingId ? " — switching now" : ""));
      onDone();
    } catch (e) {
      toast((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return { busy, run };
}

/** The YouTube video a song plays from, and YouTube's songs and videos to pick from. */
function YoutubePane({
  track,
  match,
  onMatch,
  libraryPlays,
  missing,
  onDone,
}: {
  track: Track;
  match: YoutubeMatch | null;
  onMatch: (m: YoutubeMatch) => void;
  /** A library plays this song now, not YouTube. */
  libraryPlays: LibrarySourceId | null;
  missing: boolean;
  onDone: () => void;
}) {
  const toast = useStore((s) => s.toastMsg);
  const { busy, run } = useApply(track, onDone);
  const [q, setQ] = useState(`${track.artist.split(",")[0]} ${track.title}`);
  const [results, setResults] = useState<{ track: Track; kind: "Song" | "Video" }[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [retrying, setRetrying] = useState(false);

  const search = async (query = q) => {
    if (!query.trim()) return;
    setSearching(true);
    try {
      // Songs, and videos (a demo or upload may only be a video). Either failing still shows the other.
      const [songs, videos] = await Promise.all([
        api.search(query.trim()).catch(() => [] as Track[]),
        api.searchVideos(query.trim()).catch(() => [] as Track[]),
      ]);
      const seen = new Set<string>();
      setResults(
        [...songs.map((t) => ({ track: t, kind: "Song" as const })), ...videos.map((t) => ({ track: t, kind: "Video" as const }))].filter(
          (r) => !isLibraryOnlyId(r.track.videoId) && !seen.has(r.track.videoId) && !!seen.add(r.track.videoId),
        ),
      );
    } catch (e) {
      toast((e as Error).message);
    } finally {
      setSearching(false);
    }
  };
  // Nothing found (or nothing plays it): show YouTube's results right away.
  useEffect(() => {
    if (match && (match.kind === "none" || missing)) search();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [match?.kind]);

  // A YouTube song (not from Spotify or an import): it has its own video, and may play from one picked instead.
  const own = !isDeferredId(track.videoId);
  const retry = async () => {
    setRetrying(true);
    try {
      const m = await api.retryYoutubeMatch(track.videoId);
      onMatch(m);
      if (own) {
        reloadSource(track.videoId);
        toast("Back to the song's own video");
      } else if (m.kind === "matched") {
        reloadSource(track.videoId);
        toast(`Found: ${m.track?.title ?? "a match"}`);
      } else {
        toast("Still nothing on YouTube: pick a video below");
        search();
      }
    } catch (e) {
      toast((e as Error).message);
    } finally {
      setRetrying(false);
    }
  };
  const use = (t: Track) => run(() => api.setYoutubeMatch(track.videoId, t), `Plays from YouTube: ${t.title}`);
  const current =
    match?.kind === "matched" || match?.kind === "picked"
      ? match.track ?? null
      : match?.kind === "self"
        ? match.track ?? track
        : null;
  const currentId = current?.videoId ?? null;
  const lib = libraryPlays ? SOURCE_LABELS[libraryPlays] : "";

  return (
    <>
      {libraryPlays && (
        <p className="muted small" style={{ margin: "0 8px 10px" }}>
          {lib} plays this song now: it comes before YouTube in Settings → Audio sources. To use YouTube, choose “Never use{" "}
          {lib} for this song” on the {TAB_LABELS[libraryPlays]} tab.
        </p>
      )}
      {current && (
        <div className="match-row current">
          <div className="track-main">
            <Cover src={current.thumbnail} size={44} videoId={current.videoId} />
            <div className="track-text">
              <div className="track-title">{current.title}</div>
              <div className="track-sub">
                {current.artist} · {fmtTime(current.duration)}
              </div>
            </div>
          </div>
          <span className="small muted">current</span>
        </div>
      )}
      {own && (
        <p className="muted small" style={{ margin: "8px 8px 0" }}>
          Wrong video? Search YouTube and pick the right one; this song plays from it from now on.
        </p>
      )}
      <form
        className="row gap"
        style={{ margin: "8px 0" }}
        onSubmit={(e) => {
          e.preventDefault();
          search();
        }}
      >
        <input className="input grow" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search YouTube" />
        <button className="btn" type="submit" disabled={searching}>
          {searching ? "Searching…" : "Search YouTube"}
        </button>
      </form>
      {own ? (
        match?.kind === "picked" && (
          <button className="link-btn small" style={{ margin: "0 8px 6px" }} disabled={retrying} onClick={retry}>
            {retrying ? "Switching…" : "Go back to the song's own video"}
          </button>
        )
      ) : (
        <button className="link-btn small" style={{ margin: "0 8px 6px" }} disabled={retrying} onClick={retry}>
          {retrying ? "Looking…" : "Let the app look again automatically"}
        </button>
      )}
      {searching && !results && <div className="spinner" />}
      <div className="sheet-list">
        {results?.map(({ track: t, kind }) => {
          const isCurrent = t.videoId === currentId;
          return (
            <div key={t.videoId} className={`match-row ${isCurrent ? "current" : ""}`}>
              <button className="track-main" disabled={busy || isCurrent} onClick={() => use(t)} title={isCurrent ? "Plays from this now" : "Use this"}>
                <Cover src={t.thumbnail} size={44} videoId={t.videoId} />
                <div className="track-text">
                  <div className="track-title">{t.title}</div>
                  <div className="track-sub">
                    {kind} · {t.artist} · {fmtTime(t.duration)}
                  </div>
                </div>
              </button>
              {isCurrent ? (
                <span className="small muted">current</span>
              ) : (
                <button className="btn small" disabled={busy} onClick={() => use(t)}>
                  Use this
                </button>
              )}
            </div>
          );
        })}
        {results?.length === 0 && <p className="muted small">Nothing on YouTube for that. Try fewer words.</p>}
      </div>
    </>
  );
}

/** Which copy in one library (Plex, Jellyfin or the music folder) a song plays from: automatic, picked, or none. */
function LibraryPane({
  source,
  track,
  choice,
  on,
  onDone,
}: {
  source: LibrarySourceId;
  track: Track;
  /** The current choice, or why it couldn't be loaded (null while loading). */
  choice: LibraryChoice | Error | null;
  /** The source is on in Settings → Audio sources. */
  on: boolean;
  onDone: () => void;
}) {
  const { busy, run } = useApply(track, onDone);
  const lib = libraryApi(source);
  const name = SOURCE_LABELS[source];
  const [q, setQ] = useState(track.title);
  const [results, setResults] = useState<Track[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loaded = choice instanceof Error ? null : choice;
  const loadError = choice instanceof Error ? choice.message : null;
  const itemId = (t: Track) => libraryItem(t.videoId)?.itemId ?? null;
  const use = (t: Track) => {
    const id = itemId(t);
    if (id) run(() => lib.setMatch(track.videoId, id), `Plays from ${name}: ${t.album ?? t.title}`);
  };
  const search = async () => {
    if (!q.trim()) return;
    setSearching(true);
    setError(null);
    try {
      setResults(await lib.search(q.trim()));
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSearching(false);
    }
  };

  const mode = loaded?.mode;
  const listed: { track: Track; live?: boolean }[] = results?.map((t) => ({ track: t })) ?? loaded?.candidates ?? [];
  const current = loaded?.current;
  return (
    <>
      {!on && (
        <p className="small warn-text" style={{ margin: "0 8px 10px" }}>
          {name} is turned off in Settings → Audio sources, so nothing plays from it until it's on again.
        </p>
      )}
      {loaded && (
        <div className="row gap wrap" style={{ margin: "0 8px 10px" }}>
          {mode !== "auto" && (
            <button
              className="btn small ghost"
              disabled={busy}
              onClick={() => run(() => lib.clearMatch(track.videoId, "auto"), `Back to automatic ${name} matching`)}
              title={`Let the app pick the ${name} copy`}
            >
              Back to automatic
            </button>
          )}
          {mode !== "off" && (
            <button
              className="btn small ghost"
              disabled={busy}
              onClick={() => run(() => lib.clearMatch(track.videoId, "off"), `Won't play from ${name}`)}
              title="Play this song from the next source in Settings → Audio sources"
            >
              Never use {name} for this song
            </button>
          )}
        </div>
      )}
      <p className="small muted" style={{ margin: "0 8px 10px" }}>
        {mode === "off"
          ? `Never plays from ${name}. Pick a track below to change that.`
          : mode === "picked"
            ? `Plays the ${name} track you picked. “Back to automatic” lets the app choose again.`
            : current
              ? `Plays the highlighted ${name} track when ${name} is its first source. Pick another to always use that one.`
              : loaded
                ? `No ${name} copy matched automatically. Pick one to use it.`
                : ""}
      </p>
      <form
        className="row gap"
        onSubmit={(e) => {
          e.preventDefault();
          search();
        }}
      >
        <input className="input grow" value={q} onChange={(e) => setQ(e.target.value)} aria-label={`Search ${name}`} />
        <button className="btn" type="submit" disabled={searching}>
          {searching ? "Searching…" : `Search ${TAB_LABELS[source]}`}
        </button>
      </form>
      {(error || loadError) && <p className="error small">{error ?? loadError}</p>}
      {((!choice && !error) || (searching && !results)) && <div className="spinner" />}
      <div className="sheet-list">
        {listed.map(({ track: t, live }) => {
          const isCurrent = mode !== "off" && !!current && current === itemId(t);
          return (
            <div key={t.videoId} className={`match-row ${isCurrent ? "current" : ""}`}>
              <button className="track-main" disabled={busy || isCurrent} onClick={() => use(t)} title={isCurrent ? "Plays from this now" : "Use this version"}>
                <Cover src={t.thumbnail} size={44} videoId={t.videoId} />
                <div className="track-text">
                  <div className="track-title">{t.title}</div>
                  <div className="track-sub">
                    {t.album ? `${t.album} · ` : ""}
                    {t.artist} · {fmtTime(t.duration)}
                    {live ? " · live" : ""}
                  </div>
                </div>
              </button>
              {isCurrent ? (
                <span className="small muted">current</span>
              ) : (
                <button className="btn small" disabled={busy} onClick={() => use(t)}>
                  Use this
                </button>
              )}
            </div>
          );
        })}
        {(results || loaded) && listed.length === 0 && (
          <p className="muted small">Nothing in {name} for that. Try a shorter search (just the title).</p>
        )}
      </div>
    </>
  );
}

/** Choose (or create) a saved playlist to add tracks to. */
export function PlaylistPicker() {
  const tracks = useStore((s) => s.playlistPickerTracks);
  const close = () => useStore.getState().pickPlaylist(null);
  const toast = useStore((s) => s.toastMsg);
  const [lists, setLists] = useState<PlaylistSummary[]>([]);
  const [name, setName] = useState("");

  useEffect(() => {
    if (tracks) api.playlists.list().then(setLists).catch(() => {});
  }, [tracks]);
  if (!tracks) return null;

  const add = async (id: number, label: string) => {
    try {
      await api.playlists.update(id, { append: tracks });
      toast(`Added to ${label}`);
      close();
    } catch (e) {
      toast((e as Error).message);
    }
  };
  const create = async () => {
    if (!name.trim()) return;
    try {
      await api.playlists.create(name.trim(), tracks);
      toast(`Created ${name.trim()}`);
      setName("");
      close();
    } catch (e) {
      toast((e as Error).message);
    }
  };

  return (
    <Sheet onClose={close}>
      <h3 className="sheet-title">Add {tracks.length > 1 ? `${tracks.length} tracks` : "to playlist"}</h3>
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          create();
        }}
      >
        <input className="input" placeholder="New playlist name" value={name} onChange={(e) => setName(e.target.value)} />
        <button className="btn" type="submit" disabled={!name.trim()}>
          <PlusIcon size={18} /> Create
        </button>
      </form>
      <div className="sheet-list">
        {lists.map((p) => (
          <button key={p.id} className="sheet-item" onClick={() => add(p.id, p.name)}>
            <Cover src={p.thumbnail} size={36} /> {p.name} <span className="muted">· {p.count}</span>
          </button>
        ))}
      </div>
    </Sheet>
  );
}

export function Toast() {
  const toast = useStore((s) => s.toast);
  if (!toast) return null;
  return <div className="toast">{toast.text}</div>;
}
