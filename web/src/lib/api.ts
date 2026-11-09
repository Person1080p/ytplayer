import { apiFetch } from "./transport";
import { qualityArgs } from "./quality";

export type Track = {
  videoId: string;
  title: string;
  artist: string;
  album?: string | null;
  duration?: number | null;
  thumbnail?: string | null;
};

export type QueueItem = { uid: string; track: Track; auto: boolean };
export type Queue = { items: QueueItem[]; index: number; autoplay: boolean; version: number; profile?: string };
export type InsertAt = "next" | "end" | "now";

export type HomeItem = {
  kind: "song" | "video" | "playlist" | "album" | "artist";
  title: string;
  subtitle: string;
  thumbnail?: string | null;
  videoId?: string | null;
  playlistId?: string | null;
  browseId?: string | null;
  track?: Track | null;
};
export type Shelf = { title: string; items: HomeItem[] };

export type PlaylistSummary = { id: number; name: string; count: number; updatedAt: number; thumbnail?: string | null };
export type Playlist = { id: number; name: string; tracks: Track[] };
/** A YouTube Music playlist (or album) linked on the Playlists page; opened live, not copied. */
export type LinkedPlaylist = { id: string; title: string; subtitle?: string | null; thumbnail?: string | null; addedAt: number };

/** A Spotify playlist imported from a link. Its songs have `spotify_…` ids, matched to audio when they play. */
export type SpotifyPlaylist = {
  id: string;
  name: string;
  owner?: string | null;
  thumbnail?: string | null;
  tracks: Track[];
  /** "embed": at most the first 100 songs; "export": the first 100 from Spotify, the rest from a playlist export (Chosic). */
  source: "embed" | "export";
  exportAt?: number | null;
  syncedAt: number;
  note?: string | null;
};
export type SpotifySummary = { id: string; name: string; owner?: string | null; thumbnail?: string | null; count: number; syncedAt: number };

export type LikedEntry = {
  id: number;
  provider: string;
  title: string;
  artist: string;
  album?: string | null;
  /** "deferred": added without a lookup; matched on YouTube when it first plays. */
  status: "matched" | "unmatched" | "error" | "manual" | "deferred";
  track?: Track | null;
};

export type ImportJob = {
  id: number;
  provider: string;
  status: "running" | "done" | "failed";
  total: number;
  done: number;
  matched: number;
  error?: string | null;
  note?: string | null;
  startedAt: number;
  finishedAt?: number | null;
};

/**
 * Where a track's audio comes from: the Plex or Jellyfin library, the server's music folder, a
 * recent MP3 copy of a YouTube play kept by the server, or YouTube.
 */
export type AudioSource = "plex" | "jellyfin" | "local" | "cache" | "youtube";

/** The audio sources in Settings → Audio sources (YouTube includes the server's MP3 copies). */
export type SourceId = "plex" | "jellyfin" | "local" | "youtube";
/** Sources with their own music library (everything but YouTube). */
export type LibrarySourceId = Exclude<SourceId, "youtube">;
/** One entry of the priority list: tried top to bottom, skipped when off. */
export type SourcePref = { id: SourceId; on: boolean };

/** Plain names for sources, as shown in the app. */
export const SOURCE_LABELS: Record<AudioSource, string> = {
  plex: "Plex",
  jellyfin: "Jellyfin",
  local: "Music folder",
  cache: "Cached",
  youtube: "YouTube",
};

/** How a song's Plex copy is chosen: automatically, picked by you, or never from Plex. */
export type PlexChoice = {
  mode: "auto" | "picked" | "off";
  /** Rating key of the Plex track it plays from now. */
  current?: string | null;
  /** Possible Plex copies, best match first (tracks under Plex-only ids). */
  candidates: { track: Track; score: number; live: boolean }[];
};

/** Same as `PlexChoice`, for any library source; `current` is the item id (see `libraryItem`). */
export type LibraryChoice = {
  mode: "auto" | "picked" | "off";
  current?: string | null;
  candidates: { track: Track; score: number; live: boolean }[];
};

/** What `/tracks/{id}/source` says a song plays from now. */
export type SourceInfo = {
  source: AudioSource;
  kbps?: number | null;
  /** The library track it plays from (Plex, Jellyfin or the music folder), when it's one. */
  item?: { source: LibrarySourceId; itemId: string; title: string; artist: string } | null;
};

/** A Jellyfin server connection (`/status` and `PUT /jellyfin/server`). */
export type JellyfinInfo = { connected: boolean; url?: string | null; serverName?: string | null; user?: string | null };

/** The git commit a build was made from. */
export type BuildVersion = { commit: string | null; commitShort: string | null; dirty: boolean; builtAt: number | null };

export type HistoryEntry = { playedAt: number; source: string; scrobbled: boolean; track: Track };

export type ProfileInfo = { active: "main" | "fresh"; freshPlays: number; freshStartedAt?: number | null };

export type Prefs = {
  /** All four audio sources, in priority order. */
  sources: SourcePref[];
  scrobble: boolean;
  reportYoutube: boolean;
  loveOnLastfm: boolean;
  /** Daily Last.fm loves sync: off, loves → library, or both ways. */
  lastfmSync: "off" | "pull" | "both";
  logYoutube: boolean;
  /** Spacing of import/sync requests to YouTube. */
  importPace: "original" | "normal" | "slow" | "very_slow";
  /** Hours the server keeps and plays MP3 copies of fully loaded YouTube tracks (0: off). */
  /** Hours to keep cached MP3s: 0 is off, -1 is forever. */
  audioCacheHours: number;
  /** Show the Deezer favourites import in Library → Import. */
  deezer: boolean;
};

export type YtLogEntry = {
  ts: number;
  kind: string;
  endpoint: string;
  detail: string;
  origin: string;
  outcome: string;
  status?: number | null;
  ms: number;
  attempt: number;
  error?: string | null;
};

export type YtLogWindow = {
  requests: number;
  cacheHits: number;
  timeouts: number;
  rateLimited: number;
  errors: number;
  avgMs: number;
  maxMs: number;
};

export type YtLog = {
  enabled: boolean;
  /** Average spacing between background (import/sync) requests right now. */
  backgroundGapMs: number;
  path?: string | null;
  stats: {
    lastMinute: YtLogWindow;
    last10Minutes: YtLogWindow;
    lastHour: YtLogWindow;
    peakPerMinute: number;
    byEndpoint: [string, number][];
    byOrigin: [string, number][];
    repeated: [string, number][];
  };
  recent: YtLogEntry[];
};

export type LikedState = { inLibrary: boolean; lastfmLoved?: boolean | null; liked: boolean };

export type TailscaleInfo = {
  mode: "embedded" | "host" | "none" | "";
  online: boolean;
  dnsName?: string | null;
  ips: string[];
  url?: string | null;
  hint?: string | null;
  state?: string | null;
  authUrl?: string | null;
};

export type Status = {
  ytdlp: { path?: string | null; version?: string | null };
  ytmusic: {
    authenticated: boolean;
    activeNow: boolean;
    source?: string | null;
    account?: string | null;
    syncedAt?: number | null;
    /** Why the last refresh from the server's browser failed; the saved sign-in is still used. */
    syncError?: string | null;
    /** Since when (unix seconds) YouTube treats the saved sign-in as signed out: it expired. */
    expiredSince?: number | null;
    /** The server's own browser, used through a web page; sign in there instead of in this browser. */
    signinBrowserUrl?: string | null;
  };
  /** `enabled`: connected and on in Audio sources. */
  plex: { connected: boolean; url?: string | null; loggedIn: boolean; enabled?: boolean };
  jellyfin: JellyfinInfo;
  /** The server binary's git commit at build time. */
  version?: BuildVersion | null;
  lastfm: { configured: boolean; keysFromEnv: boolean; connected: boolean; username?: string | null };
  /** Spotify playlists: where playlist exports (CSV) are picked up from. */
  spotify: { exportDirs?: string[]; exportDirSet?: boolean };
  deezer: { userId?: string | null };
  prefs: Prefs;
  /** MP3 copies of YouTube plays kept on the server. `available`: ffmpeg was found. */
  audioCache: { available: boolean; hours: number; files: number; bytes: number };
  tailscale: TailscaleInfo;
  publicUrl: string;
};

export type PlexServer = {
  name: string;
  clientIdentifier: string;
  accessToken: string;
  owned: boolean;
  connections: { uri: string; local: boolean; relay: boolean }[];
};

/** Errors that just mean "no connection yet" — the global banner covers these. */
export const isNotConnected = (e: unknown) => (e as Error)?.name === "NotConnectedError";

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function req<T>(
  path: string,
  init?: { method?: string; body?: unknown; raw?: Blob; signal?: AbortSignal },
): Promise<T> {
  const json = init?.body !== undefined;
  const res = await apiFetch(path, {
    method: init?.method ?? (json || init?.raw ? "POST" : "GET"),
    headers: json ? { "content-type": "application/json" } : init?.raw ? { "content-type": "application/octet-stream" } : undefined,
    body: json ? JSON.stringify(init!.body) : init?.raw,
    signal: init?.signal,
  });
  if (res.status === 401) window.dispatchEvent(new Event("ytp:unauthorized"));
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new ApiError(res.status, data.error ?? res.statusText);
  }
  return res.json();
}

const enc = encodeURIComponent;

/** Songs that are only in Plex use `plex_<ratingKey>` in place of a YouTube video id. */
export const isPlexId = (videoId: string) => /^plex_\d+$/.test(videoId);
/** Songs that are only in Jellyfin: `jf_<item id>`. */
export const isJellyfinId = (videoId: string) => /^jf_[0-9a-f]{32}$/.test(videoId);
/** Songs found in the music folder on a device (`local_<16 hex>`, from their artist and title).
 *  Elsewhere they play like imported songs (matched by their tags). */
export const isLocalId = (videoId: string) => /^local_[0-9a-f]{16}$/.test(videoId);
/** Songs that only exist in one of the server's libraries (no YouTube video to fall back on). */
export const isLibraryOnlyId = (videoId: string) => isPlexId(videoId) || isJellyfinId(videoId);

/** The library and item id behind a library-only song id (`plex_1` → plex, "1"), or null. */
export function libraryItem(videoId: string): { source: LibrarySourceId; itemId: string } | null {
  if (isPlexId(videoId)) return { source: "plex", itemId: videoId.slice("plex_".length) };
  if (isJellyfinId(videoId)) return { source: "jellyfin", itemId: videoId.slice("jf_".length) };
  if (isLocalId(videoId)) return { source: "local", itemId: videoId.slice("local_".length) };
  return null;
}

export type SearchResults = {
  top: HomeItem | null;
  songs: Track[];
  artists: HomeItem[];
  albums: HomeItem[];
  playlists: HomeItem[];
  videos: Track[];
  /** YouTube's search failed (this says why): the songs are from your libraries and the audio cache. */
  fallback?: string;
};
export type SearchFilter = "songs" | "artists" | "albums";

/**
 * A song's YouTube match. `self`: it is a YouTube video; `matched`: plays from `track`; `none`:
 * nothing found; `unknown`: not looked up yet; `plex`: a Plex-only song.
 */
/**
 * The YouTube video a song plays from. `self`: a YouTube song's own video; `picked`: another video
 * picked for a YouTube song; `matched`: a song from elsewhere (Spotify) matched to a video.
 */
export type YoutubeMatch = { kind: "self" | "picked" | "matched" | "none" | "unknown" | "plex" | "library"; track?: Track | null; missing: boolean };

/** Songs from other services (Spotify) are queued under their own ids and matched when they play. */
export const isSpotifyId = (videoId: string) => /^spotify_[A-Za-z0-9]{22}$/.test(videoId);
/** A song added by a library import without looking it up (matched on YouTube when it plays). */
export const isSongId = (videoId: string) => /^song_[0-9a-f]{16}$/.test(videoId);
/** No YouTube video of its own (Spotify playlist songs, imported songs): matched when played. */
export const isDeferredId = (videoId: string) => isSpotifyId(videoId) || isSongId(videoId);
export type BrowseRef = { browseId: string; params?: string | null };
export type ArtistShelf = { title: string; items: HomeItem[]; more?: BrowseRef | null };
export type ArtistPage = {
  id: string;
  name: string;
  thumbnail?: string | null;
  description?: string | null;
  stats?: string | null;
  radioPlaylistId?: string | null;
  topSongs: Track[];
  songsPlaylistId?: string | null;
  shelves: ArtistShelf[];
};
export type GridPage = { title: string; items: HomeItem[] };
/** A page of the home feed; `continuation` fetches the next one (absent at the end). */
export type HomeFeed = { shelves: Shelf[]; continuation?: string };
/** A page of tracks (a podcast's episodes); `continuation` fetches the next one (null at the end). */
export type TrackPage = { tracks: Track[]; continuation?: string | null };

export const api = {
  home: (refresh = false) => req<HomeFeed>(`/home${refresh ? "?refresh=1" : ""}`),
  /** The next shelves of the home feed, from the previous page's `continuation`. */
  homeMore: (token: string) => req<HomeFeed>(`/home/more?token=${enc(token)}`),
  /** A podcast's newest episodes; `continuation` fetches the next page (one YouTube call each). */
  podcast: (id: string) => req<TrackPage>(`/podcast/${enc(id)}`),
  podcastMore: (id: string, token: string) => req<TrackPage>(`/podcast/${enc(id)}/more?token=${enc(token)}`),
  discover: () => req<{ shelves: Shelf[] }>("/discover"),
  search: (q: string, signal?: AbortSignal) => req<Track[]>(`/search?q=${enc(q)}`, { signal }),
  /** YouTube videos only (uploads that aren't in the song catalogue: demos, mixes, live sets). */
  searchVideos: (q: string) => req<Track[]>(`/search/videos?q=${enc(q)}`),
  searchAll: (q: string, filter?: SearchFilter, signal?: AbortSignal) =>
    req<SearchResults>(`/search/all?q=${enc(q)}${filter ? `&filter=${filter}` : ""}`, { signal }),
  artist: (id: string) => req<ArtistPage>(`/artist/${enc(id)}`),
  artistLookup: (name: string) => req<{ id: string | null }>(`/artist-lookup?name=${enc(name)}`),
  browseGrid: (id: string, params?: string | null) =>
    req<GridPage>(`/browse/grid?id=${enc(id)}${params ? `&params=${enc(params)}` : ""}`),
  radio: (videoId: string) => req<Track[]>(`/radio/${enc(videoId)}`),
  playlist: (id: string) => req<Track[]>(`/playlist/${enc(id)}`),
  track: (videoId: string) => req<Track>(`/tracks/${enc(videoId)}`),
  playlistInfo: (id: string) =>
    req<{ title?: string | null; artist?: string | null; thumbnail?: string | null }>(`/playlist/${enc(id)}/info`),
  recommendations: (videoId: string) => req<Track[]>(`/recommendations/${enc(videoId)}`),
  /** `only`: just these sources (the web app asks about the ones ahead of its music folder). */
  source: (videoId: string, opts: { only?: SourceId[] } = {}) =>
    req<SourceInfo>(withArgs(`/tracks/${enc(videoId)}/source`, [...qualityArgs(), ...(opts.only ? [`only=${opts.only.join(",")}`] : [])])),
  liked: (videoId: string) => req<LikedState>(`/tracks/${enc(videoId)}/liked`),
  setLiked: (track: Track, liked: boolean) =>
    req<LikedState>(`/tracks/${enc(track.videoId)}/liked`, { method: "PUT", body: { track, liked } }),
  history: () => req<HistoryEntry[]>("/history"),
  playback: (ev: {
    sessionId: string;
    track: Track;
    event: "start" | "progress" | "end";
    position: number;
    duration?: number;
    source?: string | null;
  }) => req<{ ok: boolean }>("/playback", { body: ev }),

  queue: {
    get: () => req<Queue>("/queue"),
    add: (tracks: Track[], at: InsertAt = "end") => req<Queue>("/queue/add", { body: { tracks, at } }),
    replace: (tracks: Track[], index = 0) => req<Queue>("/queue/replace", { body: { tracks, index } }),
    radio: (track: Track) => req<Queue>("/queue/radio", { body: { track } }),
    setIndex: (index: number) => req<Queue>("/queue/index", { body: { index } }),
    move: (from: number, to: number) => req<Queue>("/queue/move", { body: { from, to } }),
    remove: (uid: string) => req<Queue>("/queue/remove", { body: { uid } }),
    clear: (all = false) => req<Queue>("/queue/clear", { body: { all } }),
    shuffle: () => req<Queue>("/queue/shuffle", { method: "POST" }),
    autoplay: (enabled: boolean) => req<Queue>("/queue/autoplay", { body: { enabled } }),
  },

  playlists: {
    list: () => req<PlaylistSummary[]>("/playlists"),
    get: (id: number) => req<Playlist>(`/playlists/${id}`),
    create: (name: string, tracks: Track[] = []) => req<Playlist>("/playlists", { body: { name, tracks } }),
    update: (id: number, body: { name?: string; tracks?: Track[]; append?: Track[] }) =>
      req<Playlist>(`/playlists/${id}`, { method: "PUT", body }),
    remove: (id: number) => req<{ ok: boolean }>(`/playlists/${id}`, { method: "DELETE" }),
    /** YouTube Music playlists and albums linked on the Playlists page. */
    linked: () => req<LinkedPlaylist[]>("/linked-playlists"),
    link: (p: Omit<LinkedPlaylist, "addedAt">) => req<LinkedPlaylist>("/linked-playlists", { body: p }),
    unlink: (id: string) => req<{ ok: boolean }>(`/linked-playlists/${enc(id)}`, { method: "DELETE" }),
    spotify: () => req<SpotifySummary[]>("/spotify-playlists"),
    /** Import a Spotify playlist from its link (reads the list only; songs are matched when they play). */
    spotifyImport: (url: string) => req<SpotifyPlaylist>("/spotify-playlists", { body: { url } }),
    /** A Spotify playlist, synced with Spotify again first when that was a while ago (or `refresh`). */
    spotifyGet: (id: string, refresh = false) => req<SpotifyPlaylist>(`/spotify-playlists/${enc(id)}${refresh ? "?refresh=1" : ""}`),
    spotifyRemove: (id: string) => req<{ ok: boolean }>(`/spotify-playlists/${enc(id)}`, { method: "DELETE" }),
    /** The full list from a playlist export CSV (songs past the first 100 Spotify's public page shows). */
    spotifyExport: (id: string, csv: string) => req<SpotifyPlaylist>(`/spotify-playlists/${enc(id)}/export`, { body: { csv } }),
    /** Folder(s) the server watches for Exportify downloads (empty: the Downloads folders). */
    spotifyExportDir: (dir: string) => req<{ ok: boolean }>("/spotify/export-dir", { method: "PUT", body: { dir } }),
  },

  library: {
    liked: (params: { provider?: string; status?: string; q?: string } = {}) => {
      const qs = new URLSearchParams(Object.entries(params).filter(([, v]) => v) as [string, string][]);
      return req<LikedEntry[]>(`/library/liked?${qs}`);
    },
    jobs: () => req<ImportJob[]>("/import/jobs"),
    ids: () => req<string[]>("/library/ids"),
    add: (track: Track) => req<{ ok: boolean }>("/library/liked", { body: { track } }),
    remove: (id: number) => req<{ ok: boolean }>(`/library/liked/${id}`, { method: "DELETE" }),
    match: (id: number, track: Track) => req<{ ok: boolean }>(`/library/liked/${id}/match`, { body: { track } }),
    rematch: () => req<{ jobId: number }>("/library/rematch", { method: "POST" }),
    importLastfm: () => req<{ jobId: number }>("/import/lastfm", { method: "POST" }),
    syncLastfm: () => req<{ jobId: number }>("/library/sync-lastfm", { method: "POST" }),
    importYtMusic: () => req<{ jobId: number }>("/import/ytmusic", { method: "POST" }),
    /** `lazy`: songs are matched on YouTube when they first play, not all looked up now. */
    importFile: (file: File, provider: string, opts: { lazy?: boolean } = {}) =>
      req<{ jobId: number }>(
        `/import/file?provider=${enc(provider)}&name=${enc(file.name)}${opts.lazy ? "&lazy=true" : ""}`,
        { raw: file },
      ),
    importDeezer: (userId: string, accessToken?: string) =>
      req<{ jobId: number }>("/import/deezer", { body: { userId, accessToken } }),
    /** `lazy`: songs are matched on YouTube when they first play, not all looked up now. */
    importText: (content: string, opts: { provider?: string; lazy?: boolean } = {}) =>
      req<{ jobId: number }>("/import/text", { body: { content, provider: opts.provider, lazy: !!opts.lazy } }),
  },

  profile: {
    get: () => req<ProfileInfo>("/profile"),
    set: (active: "main" | "fresh") => req<ProfileInfo>("/profile", { method: "PUT", body: { active } }),
    resetFresh: () => req<ProfileInfo>("/profile/fresh/reset", { method: "POST" }),
  },

  status: () => req<Status>("/status"),
  setPrefs: (p: Prefs) => req<Prefs>("/prefs", { method: "PUT", body: p }),
  youtubeLog: () => req<YtLog>("/logs/youtube"),
  clearYoutubeLog: () => req<{ ok: boolean }>("/logs/youtube", { method: "DELETE" }),
  youtubeLogFile: async () => {
    const res = await apiFetch("/logs/youtube/file", { method: "GET" });
    if (!res.ok) throw new ApiError(res.status, res.statusText);
    return res.blob();
  },
  updateYtdlp: () => req<{ output: string; version?: string }>("/ytdlp/update", { method: "POST" }),
  setYtCookie: (cookie: string) => req<{ ok: boolean; account?: string }>("/ytmusic/cookie", { method: "PUT", body: { cookie } }),
  importYtFromBrowser: (keepInSync: boolean, browser?: string) =>
    req<{ ok: boolean; browser: string; account: string }>("/ytmusic/import-browser", { body: { keepInSync, browser } }),
  syncYt: () => req<{ ok: boolean; synced: boolean }>("/ytmusic/sync", { method: "POST" }),
  deleteYtCookie: () => req<{ ok: boolean }>("/ytmusic/cookie", { method: "DELETE" }),
  setLastfmKeys: (apiKey: string, apiSecret: string) =>
    req<{ ok: boolean }>("/lastfm/keys", { method: "PUT", body: { apiKey, apiSecret } }),
  lastfmLogin: (callback: string) => req<{ url: string }>(`/lastfm/login?callback=${enc(callback)}`),
  lastfmSession: (token: string) => req<{ ok: boolean; username: string }>("/lastfm/session", { body: { token } }),
  lastfmDisconnect: () => req<{ ok: boolean }>("/lastfm", { method: "DELETE" }),
  plexPin: () => req<{ id: number; code: string; authUrl: string }>("/plex/pin", { method: "POST" }),
  plexPinCheck: (id: number) => req<{ authorized: boolean }>(`/plex/pin/${id}`),
  plexServers: () => req<PlexServer[]>("/plex/servers"),
  plexSetServer: (url: string, token: string) => req<{ ok: boolean }>("/plex/server", { method: "PUT", body: { url, token } }),
  plexDisconnect: () => req<{ ok: boolean }>("/plex", { method: "DELETE" }),
  /** Plex tracks as songs; their ids are `plex_<ratingKey>`. */
  plexSearch: (q: string) => req<Track[]>(`/plex/search?q=${enc(q)}`),
  /** Play a YouTube-matched song from this Plex track. */
  plexSetMatch: (videoId: string, ratingKey: string) =>
    req<{ ratingKey: string }>(`/plex/matches/${enc(videoId)}`, { method: "PUT", body: { ratingKey } }),
  /** Which Plex track a YouTube song plays from, and the Plex copies to choose from. */
  plexMatch: (videoId: string) => req<PlexChoice>(`/plex/matches/${enc(videoId)}`),
  /** `off`: never play this song from Plex; `auto`: back to automatic matching. */
  plexClearMatch: (videoId: string, mode: "off" | "auto") =>
    req<{ ok: boolean }>(`/plex/matches/${enc(videoId)}?mode=${mode}`, { method: "DELETE" }),

  jellyfin: {
    connect: (url: string, username: string, password: string) =>
      req<JellyfinInfo>("/jellyfin/server", { method: "PUT", body: { url, username, password } }),
    disconnect: () => req<{ ok: boolean }>("/jellyfin", { method: "DELETE" }),
    /** Jellyfin tracks as songs; their ids are `jf_<item id>`. */
    search: (q: string) => req<Track[]>(`/jellyfin/search?q=${enc(q)}`),
    match: (videoId: string) => req<LibraryChoice>(`/jellyfin/matches/${enc(videoId)}`),
    setMatch: (videoId: string, itemId: string) =>
      req<unknown>(`/jellyfin/matches/${enc(videoId)}`, { method: "PUT", body: { itemId } }),
    clearMatch: (videoId: string, mode: "off" | "auto") =>
      req<{ ok: boolean }>(`/jellyfin/matches/${enc(videoId)}?mode=${mode}`, { method: "DELETE" }),
  },


  /** Songs from every server library that's on (Plex, Jellyfin; not this device's folder). */
  librarySearch: (q: string) => req<Track[]>(`/sources/search?q=${enc(q)}`),

  /** Which YouTube video a song from another service (Spotify) plays from. */
  youtubeMatch: (videoId: string) => req<YoutubeMatch>(`/tracks/${enc(videoId)}/youtube`),
  /** Play that song from this YouTube video (a song or video from YouTube search). */
  setYoutubeMatch: (videoId: string, track: Track) =>
    req<YoutubeMatch>(`/tracks/${enc(videoId)}/youtube`, { method: "PUT", body: { track } }),
  /** Look for a YouTube match again now (also after "nothing found"); a YouTube song goes back to its own video. */
  retryYoutubeMatch: (videoId: string) => req<YoutubeMatch>(`/tracks/${enc(videoId)}/youtube/retry`, { method: "POST" }),
  /** Songs nothing can play right now. */
  missing: () => req<string[]>("/missing"),
  authCheck: () => req<{ authenticated: boolean; required: boolean }>("/auth/check"),
  login: (token: string) => req<{ ok: boolean }>("/auth/login", { body: { token } }),
};

/** Matching calls for one library source (Plex keeps its own endpoints). */
export type LibraryApi = {
  search: (q: string) => Promise<Track[]>;
  match: (videoId: string) => Promise<LibraryChoice>;
  setMatch: (videoId: string, itemId: string) => Promise<unknown>;
  clearMatch: (videoId: string, mode: "off" | "auto") => Promise<{ ok: boolean }>;
};

/** The music folder on this device (lib/localFolder.ts registers it; kept out of this module so
 *  the two don't import each other). */
let localLibrary: LibraryApi | null = null;
export function registerLocalLibrary(a: LibraryApi) {
  localLibrary = a;
}

export function libraryApi(source: LibrarySourceId): LibraryApi {
  if (source === "local") {
    if (!localLibrary) throw new Error("The music folder isn't available in this browser");
    return localLibrary;
  }
  if (source !== "plex") return api[source];
  return { search: api.plexSearch, match: api.plexMatch, setMatch: api.plexSetMatch, clearMatch: api.plexClearMatch };
}

/** Raw audio response for a track (used when audio can't be loaded by URL, i.e. over the in-page tailnet). */
function withArgs(path: string, args: string[]) {
  return args.length ? `${path}?${args.join("&")}` : path;
}

/** `/stream/:id` with the current quality budget (see quality.ts). */
function streamPath(videoId: string, via?: "youtube" | "pipe") {
  const rev = sourceRevs[videoId];
  return withArgs(`/stream/${enc(videoId)}`, [...(via ? [`via=${via}`] : []), ...qualityArgs(), ...(rev ? [`rev=${rev}`] : [])]);
}

// Songs whose audio source just changed (a Plex version picked or turned off). Their stream URLs
// get a `rev`, since the browser may keep a Plex stream for an hour under the old URL. Kept a
// couple of hours (longer than that cache) and across reloads.
const REV_KEY = "ytp.sourceRev";
const REV_KEEP_MS = 2 * 3600_000;
let sourceRevs: Record<string, number> = (() => {
  try {
    return JSON.parse(localStorage.getItem(REV_KEY) ?? "{}") as Record<string, number>;
  } catch {
    return {};
  }
})();

/** `videoId` now plays from a different source: don't reuse a stream the browser kept. */
export function bumpSourceRev(videoId: string) {
  const now = Date.now();
  sourceRevs = Object.fromEntries(Object.entries(sourceRevs).filter(([, t]) => now - t < REV_KEEP_MS));
  sourceRevs[videoId] = now;
  try {
    localStorage.setItem(REV_KEY, JSON.stringify(sourceRevs));
  } catch {
    // private mode: this session still gets fresh URLs
  }
}

export function fetchAudio(videoId: string, via?: "youtube" | "pipe", signal?: AbortSignal) {
  return apiFetch(streamPath(videoId, via), { signal });
}

export function streamUrl(videoId: string, via?: "youtube" | "pipe") {
  return `/api${streamPath(videoId, via)}`;
}

export function fmtTime(sec?: number | null) {
  if (sec == null || !isFinite(sec)) return "–:––";
  const s = Math.max(0, Math.floor(sec));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

export function randomId() {
  // crypto.randomUUID is only available in secure contexts (not plain http://100.x.y.z).
  return Array.from({ length: 16 }, () => Math.floor(Math.random() * 36).toString(36)).join("");
}
