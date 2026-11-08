import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  api,
  fmtTime,
  isPlexId,
  isSpotifyId,
  type PlaylistSummary,
  type PlexChoice,
  type Track,
  type YoutubeMatch,
} from "../lib/api";
import { setLiked, useLibrary } from "../lib/library";
import { enqueue, playRadio, reloadSource } from "../lib/player";
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
      <button
        className="sheet-item"
        onClick={act(() => {
          useStore.getState().setNowPlaying(false);
          nav(artistLink(track.artist));
        })}
      >
        <SearchIcon size={20} /> Go to {track.artist.split(",")[0]}
      </button>
      {!isPlexId(track.videoId) && (
        <button className="sheet-item" onClick={() => useStore.getState().choosePlexVersion(track)}>
          <MatchIcon size={20} /> Change audio source…
        </button>
      )}
    </Sheet>
  );
}

// Whether a Plex server is connected, asked once per session (and again after a failure).
let plexConnected: Promise<boolean> | null = null;
function usePlexConnected(active: boolean) {
  const [on, setOn] = useState(false);
  useEffect(() => {
    if (!active) return;
    plexConnected ??= api.status().then(
      (s) => s.plex.connected,
      () => ((plexConnected = null), false),
    );
    let live = true;
    plexConnected.then((v) => live && setOn(v));
    return () => {
      live = false;
    };
  }, [active]);
  return on;
}

/**
 * Change where a song's audio comes from. Two tabs:
 * - YouTube (songs from Spotify): the video it's matched to, a search of YouTube's songs and
 *   videos to pick another, or "search again" after nothing was found.
 * - Plex: automatic matching, a Plex copy you pick (the studio recording rather than a live one),
 *   or not Plex at all.
 * Changes apply straight away; the song switches over in place if it's playing.
 */
export function PlexVersionSheet() {
  const track = useStore((s) => s.plexMatchTrack);
  const plexOn = usePlexConnected(!!track);
  const missing = useStore((s) => (track ? s.missing.has(track.videoId) : false));
  const close = () => useStore.getState().choosePlexVersion(null);
  const [tab, setTab] = useState<"youtube" | "plex">("youtube");
  // Picked a tab by hand: don't switch to Plex when its answer arrives.
  const tabTouched = useRef(false);
  const [plexChoice, setPlexChoice] = useState<PlexChoice | null>(null);
  const [yt, setYt] = useState<YoutubeMatch | null>(null);

  const spotify = !!track && isSpotifyId(track.videoId);
  const youtubeTab = !!track && !isPlexId(track.videoId);
  const plexTab = plexOn && youtubeTab;
  useEffect(() => {
    setPlexChoice(null);
    setYt(null);
    tabTouched.current = false;
    setTab("youtube");
    if (track && !isPlexId(track.videoId)) api.youtubeMatch(track.videoId).then(setYt, () => {});
  }, [track]);
  useEffect(() => {
    if (!track || !plexTab) return;
    api.plexMatch(track.videoId).then((c) => {
      setPlexChoice(c);
      // A Plex copy is what plays: open on the tab that changes it.
      if (!tabTouched.current && c.current && c.mode !== "off") setTab("plex");
    }, () => {});
  }, [track, plexTab]);
  if (!track) return null;

  const plexPlays = !!plexChoice?.current && plexChoice.mode !== "off";
  const plexCurrent = plexPlays ? plexChoice?.candidates.find((c) => c.track.videoId === `plex_${plexChoice.current}`)?.track : null;
  const tabs = [...(youtubeTab ? (["youtube"] as const) : []), ...(plexTab ? (["plex"] as const) : [])];

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
      <p className={`small source-now ${missing ? "missing" : ""}`}>
        {missing && !plexPlays
          ? "No audio found for this song. Pick a source below."
          : plexPlays
            ? `Plays from Plex${plexCurrent ? `: ${plexCurrent.album ?? plexCurrent.title}` : ""}.`
            : yt?.kind === "picked"
              ? `Plays from a YouTube video you picked${yt.track ? `: ${yt.track.title} · ${yt.track.artist}` : ""}.`
              : !spotify
              ? "Plays from YouTube (its own video)."
              : yt?.kind === "matched" && yt.track
                ? `Plays from YouTube: ${yt.track.title} · ${yt.track.artist}.`
                : yt?.kind === "none"
                  ? "Nothing was found on YouTube. Pick a video below."
                  : yt
                    ? "Found on YouTube the first time it plays, or pick one below."
                    : "…"}
      </p>
      {tabs.length > 1 && (
        <div className="tabs" role="tablist" style={{ margin: "0 8px 10px" }}>
          {tabs.map((t) => (
            <button
              key={t}
              role="tab"
              aria-selected={tab === t}
              className={tab === t ? "on" : ""}
              onClick={() => {
                tabTouched.current = true;
                setTab(t);
              }}
            >
              {t === "youtube" ? "YouTube" : "Plex"}
            </button>
          ))}
        </div>
      )}
      {tab === "youtube" && youtubeTab ? (
        <YoutubePane track={track} match={yt} onMatch={setYt} plexPlays={plexPlays} missing={missing} onDone={close} />
      ) : plexTab ? (
        <PlexPane track={track} choice={plexChoice} onDone={close} />
      ) : (
        <p className="muted small" style={{ margin: "0 8px" }}>
          Connect Plex in Settings to play this song from your Plex library.
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

/** A song from Spotify: the YouTube video it plays from, and YouTube's songs and videos to pick from. */
function YoutubePane({
  track,
  match,
  onMatch,
  plexPlays,
  missing,
  onDone,
}: {
  track: Track;
  match: YoutubeMatch | null;
  onMatch: (m: YoutubeMatch) => void;
  plexPlays: boolean;
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
          (r) => !seen.has(r.track.videoId) && !!seen.add(r.track.videoId),
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

  // A YouTube song (not from Spotify): it has its own video, and may play from one picked instead.
  const own = !isSpotifyId(track.videoId);
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

  return (
    <>
      {plexPlays && (
        <p className="muted small" style={{ margin: "0 8px 10px" }}>
          A Plex copy plays first while one is matched (Plex tab → “Don't use Plex” to use YouTube).
        </p>
      )}
      {current && (
        <div className="match-row current">
          <div className="track-main">
            <Cover src={current.thumbnail} size={44} />
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
                <Cover src={t.thumbnail} size={44} />
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

/** Which Plex copy a song plays from: automatic, picked, or none. */
function PlexPane({ track, choice, onDone }: { track: Track; choice: PlexChoice | null; onDone: () => void }) {
  const { busy, run } = useApply(track, onDone);
  const [q, setQ] = useState(track.title);
  const [results, setResults] = useState<Track[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const use = (t: Track) =>
    run(() => api.plexSetMatch(track.videoId, t.videoId.slice("plex_".length)), `Plays from Plex: ${t.album ?? t.title}`);
  const search = async () => {
    if (!q.trim()) return;
    setResults([]);
    try {
      setResults(await api.plexSearch(q.trim()));
    } catch (e) {
      setError((e as Error).message);
      setResults(null);
    }
  };

  const mode = choice?.mode;
  const listed: { track: Track; live?: boolean }[] = results?.map((t) => ({ track: t })) ?? choice?.candidates ?? [];
  const current = choice?.current;
  return (
    <>
      {choice && (
        <div className="row gap wrap" style={{ margin: "0 8px 10px" }}>
          <button
            className={`btn small ${mode === "auto" ? "" : "ghost"}`}
            disabled={busy || mode === "auto"}
            onClick={() => run(() => api.plexClearMatch(track.videoId, "auto"), "Back to automatic Plex matching")}
            title="Let the app pick the Plex copy"
          >
            Automatic
          </button>
          <button
            className={`btn small ${mode === "off" ? "" : "ghost"}`}
            disabled={busy || mode === "off"}
            onClick={() => run(() => api.plexClearMatch(track.videoId, "off"), "Won't play from Plex")}
            title="Play this song from the audio cache or YouTube instead"
          >
            Don't use Plex
          </button>
        </div>
      )}
      <p className="small muted" style={{ margin: "0 8px 10px" }}>
        {mode === "off"
          ? "Never plays from Plex. Pick a track below to change that."
          : mode === "picked"
            ? "Plays the Plex track you picked. “Automatic” lets the app choose again."
            : current
              ? "Plays the highlighted Plex track. Pick another to always use that one."
              : choice
                ? "No Plex copy matched automatically. Pick one to use it."
                : ""}
      </p>
      <form
        className="row gap"
        onSubmit={(e) => {
          e.preventDefault();
          search();
        }}
      >
        <input className="input grow" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search Plex" />
        <button className="btn" type="submit">
          Search Plex
        </button>
      </form>
      {error && <p className="error small">{error}</p>}
      {!choice && !error && <div className="spinner" />}
      <div className="sheet-list">
        {listed.map(({ track: t, live }) => {
          const isCurrent = mode !== "off" && current === t.videoId.slice("plex_".length);
          return (
            <div key={t.videoId} className={`match-row ${isCurrent ? "current" : ""}`}>
              <button className="track-main" disabled={busy || isCurrent} onClick={() => use(t)} title={isCurrent ? "Plays from this now" : "Use this version"}>
                <Cover src={t.thumbnail} size={44} />
                <div className="track-text">
                  <div className="track-title">{t.title}</div>
                  <div className="track-sub">
                    {t.album ?? t.artist} · {fmtTime(t.duration)}
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
        {choice && listed.length === 0 && <p className="muted small">Nothing in Plex for that. Try a shorter search (just the title).</p>}
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
