import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { api, isNotConnected, type HomeItem, type SearchFilter, type SearchResults, type Track } from "../lib/api";
import { linkPath, parseYouTubeLink } from "../lib/links";
import { enqueue, playRadio } from "../lib/player";
import { TrackList, Cover } from "../components/TrackRow";
import { SearchIcon } from "../components/Icons";
import { Card, itemLink, ShelfView } from "../components/Shelf";
import { useYoutubeOn } from "../lib/serverStatus";
import { folderActive, localApi } from "../lib/localFolder";
import { normalize, primaryArtist } from "../lib/matching";

type Tab = "all" | SearchFilter;
const TABS: { id: Tab; label: string }[] = [
  { id: "all", label: "All" },
  { id: "songs", label: "Songs" },
  { id: "artists", label: "Artists" },
  { id: "albums", label: "Albums" },
];

/** The best match, shown big: an artist, album or song. */
function TopResult({ item, label: heading }: { item: HomeItem; label?: string }) {
  const nav = useNavigate();
  const link = itemLink(item);
  const kind = { artist: "Artist", album: "Album", playlist: "Playlist", song: "Song", video: "Video" }[item.kind];
  const label = heading ?? `Top result · ${kind}`;
  return (
    <button
      className="top-result"
      onClick={() => (item.track ? playRadio(item.track) : link && nav(link))}
    >
      <Cover src={item.thumbnail} size={96} round={item.kind === "artist"} />
      <div className="top-result-text">
        <div className="small muted">{label}</div>
        <div className="top-result-title">{item.title}</div>
        {item.subtitle && <div className="small muted">{item.subtitle}</div>}
      </div>
    </button>
  );
}

function Grid({ items }: { items: HomeItem[] }) {
  return (
    <div className="card-grid">
      {items.map((it, i) => (
        <Card key={i} item={it} size={140} />
      ))}
    </div>
  );
}

/** Songs from the music folder on this device (when it's set up and on). */
function folderSongs(query: string): Promise<Track[]> {
  return folderActive() ? localApi.search(query).catch(() => []) : Promise.resolve([]);
}

/** The folder's songs first, without the server's copies of the same songs. */
function withFolder(r: SearchResults, local: Track[]): SearchResults {
  if (!local.length) return r;
  const key = (t: Track) => `${primaryArtist(t.artist)}|${normalize(t.title)}`;
  const seen = new Set(local.map(key));
  return { ...r, songs: [...local, ...r.songs.filter((t) => !seen.has(key(t)))] };
}

const NO_RESULTS: SearchResults = { top: null, songs: [], artists: [], albums: [], playlists: [], videos: [] };

export default function Search() {
  const nav = useNavigate();
  const [params, setParams] = useSearchParams();
  const [q, setQ] = useState(params.get("q") ?? "");
  const [pickedTab, setTab] = useState<Tab>((params.get("tab") as Tab) || "all");
  // With YouTube off, search covers the songs in your libraries only (no artists or albums).
  const youtube = useYoutubeOn();
  const tabs = youtube ? TABS : TABS.filter((t) => t.id === "all" || t.id === "songs");
  const tab: Tab = tabs.some((t) => t.id === pickedTab) ? pickedTab : "all";
  const [results, setResults] = useState<SearchResults | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  // A YouTube link in the box: playlists, albums and channels open directly; a video is shown
  // as the one result (playing needs a tap, which iOS requires anyway).
  const link = useMemo(() => (youtube ? parseYouTubeLink(q) : null), [q, youtube]);
  const [linkTrack, setLinkTrack] = useState<Track | null>(null);

  useEffect(() => {
    if (!params.get("q")) input.current?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    setLinkTrack(null);
    if (!link) return;
    setResults(null);
    if (link.kind !== "video") {
      // A short wait, so a link being typed out isn't opened half-way. Not written to the URL,
      // so going back returns to the search page without re-opening it.
      const t = setTimeout(() => {
        setQ("");
        nav(linkPath(link));
      }, 400);
      return () => clearTimeout(t);
    }
    let alive = true;
    setLoading(true);
    setError(null);
    api
      .track(link.videoId)
      .then((t) => alive && setLinkTrack(t))
      .catch((e) => alive && !isNotConnected(e) && setError(`Couldn't load that video: ${e.message}`))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [link, nav]);

  useEffect(() => {
    const query = q.trim();
    if (!query || link) {
      setResults(null);
      return;
    }
    const ctrl = new AbortController();
    // One request per (debounced) query and tab; the server caches each for hours.
    const t = setTimeout(() => {
      setLoading(true);
      setError(null);
      setParams(tab === "all" ? { q: query } : { q: query, tab }, { replace: true });
      const local = tab === "all" || tab === "songs" ? folderSongs(query) : Promise.resolve([]);
      api
        .searchAll(query, tab === "all" ? undefined : tab, ctrl.signal)
        .then(async (res) => !ctrl.signal.aborted && setResults(withFolder(res, await local)))
        .catch(async (e) => {
          if (e.name === "AbortError") return;
          // The server couldn't search (e.g. every source of its own is off): the folder may still have it.
          const songs = await local;
          if (songs.length) return setResults({ ...NO_RESULTS, songs });
          if (!isNotConnected(e)) setError(e.message);
        })
        .finally(() => setLoading(false));
    }, 350);
    return () => {
      clearTimeout(t);
      ctrl.abort();
    };
  }, [q, tab, setParams, link]);

  const r = results;
  const empty =
    r && !r.top && !r.songs.length && !r.artists.length && !r.albums.length && !r.playlists.length && !r.videos.length;
  const artists = r ? [...(r.top?.kind === "artist" && tab === "artists" ? [r.top] : []), ...r.artists] : [];
  const albums = r ? [...(r.top?.kind === "album" && tab === "albums" ? [r.top] : []), ...r.albums] : [];

  return (
    <div className="page">
      <div className="search-box">
        <SearchIcon size={20} />
        <input
          ref={input}
          className="input"
          type="search"
          placeholder={youtube ? "Songs, artists, albums, or a YouTube link" : "Songs in your libraries"}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && linkTrack) playRadio(linkTrack);
          }}
        />
      </div>
      {link?.kind === "video" && (
        <>
          {linkTrack && (
            <TopResult
              item={{
                kind: "song",
                title: linkTrack.title,
                subtitle: linkTrack.artist,
                thumbnail: linkTrack.thumbnail,
                videoId: linkTrack.videoId,
                track: linkTrack,
              }}
              label="From the link"
            />
          )}
          {link.list && (
            <button className="btn ghost" onClick={() => nav(linkPath({ kind: "playlist", id: link.list! }))}>
              Open the playlist this link is from
            </button>
          )}
        </>
      )}
      {q.trim() && !link && (
        <div className="tabs search-tabs" role="tablist">
          {tabs.map((t) => (
            <button
              key={t.id}
              role="tab"
              aria-selected={tab === t.id}
              className={tab === t.id ? "on" : ""}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>
      )}
      {loading && <div className="spinner" />}
      {error && <p className="error">{error}</p>}
      {r && !youtube && (
        <p className="notice small">YouTube is off (Settings → Audio sources), so these are songs from your libraries.</p>
      )}
      {r?.fallback && youtube && (
        <p className="notice small" title={r.fallback}>
          YouTube search isn't working right now, so these are songs from your libraries and the server's audio cache.
        </p>
      )}

      {r && tab === "all" && (
        <>
          {r.top && <TopResult item={r.top} />}
          {r.songs.length > 0 && (
            <section className="shelf">
              <div className="shelf-head">
                <h2>Songs</h2>
                <button className="btn small ghost" onClick={() => setTab("songs")}>
                  See all
                </button>
              </div>
              <TrackList tracks={r.songs.slice(0, 5)} onPlay={(i) => playRadio(r.songs[i])} />
            </section>
          )}
          {r.artists.length > 0 && <ShelfView shelf={{ title: "Artists", items: r.artists }} />}
          {r.albums.length > 0 && <ShelfView shelf={{ title: "Albums", items: r.albums }} />}
          {r.videos.length > 0 && (
            <ShelfView
              shelf={{
                title: "Videos",
                items: r.videos.map((t) => ({ kind: "video", title: t.title, subtitle: t.artist, thumbnail: t.thumbnail, videoId: t.videoId, track: t })),
              }}
            />
          )}
          {r.playlists.length > 0 && <ShelfView shelf={{ title: "Playlists", items: r.playlists }} />}
        </>
      )}

      {r && tab === "songs" && r.songs.length > 0 && (
        <>
          <div className="row gap">
            <button className="btn small ghost" onClick={() => enqueue(r.songs, "end")}>
              Add all to queue
            </button>
          </div>
          <TrackList tracks={r.songs} onPlay={(i) => playRadio(r.songs[i])} />
        </>
      )}
      {r && tab === "artists" && <Grid items={artists} />}
      {r && tab === "albums" && <Grid items={albums} />}

      {!loading && empty && !error && <p className="muted">{!youtube ? "Nothing matching in your libraries." : r?.fallback ? "Nothing matching in your libraries or the audio cache." : "No results."}</p>}
      {!q && (
        <p className="muted">
          {youtube ? "Find songs, artists and albums." : "Find songs in your libraries."} Tapping a song plays it with an automatic
          radio; use ⋯ to queue it instead.
        </p>
      )}
    </div>
  );
}
