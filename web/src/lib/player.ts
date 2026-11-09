// Audio engine. The queue lives on the server (shared by all devices); this module
// owns the <audio> elements on *this* device, follows the queue's current item,
// reports listening progress (scrobbles / YouTube history) and drives the Media Session.
//
// Direct mode streams `/api/stream/:id` by URL (with Range/seeking).
// In-page tailnet mode can't hand a URL to <audio>, so each track is downloaded over
// the tailnet into a Blob, and the next track is prefetched — which also keeps
// auto-advance working while iOS has the app in the background.
//
// Two elements, where the browser allows it: once the current song has finished loading,
// the next song in the queue loads into the spare element, and moving on just switches
// elements (no request to wait for). The same pair does crossfades. iOS ignores
// `volume` and only lets an element that has already played auto-advance in the
// background, so there a single element is kept and only the download is done ahead.

import { api, bumpSourceRev, fetchAudio, hlsUrl, randomId, streamUrl, type AudioSource, type InsertAt, type SourceInfo, type Queue, type QueueItem, type Track } from "./api";
import { invalidateLocal, localUrl, needsDecision, prepareLocal, skipLocal } from "./localFolder";
import { useStore } from "./store";
import { apiFetch, detectConnection, isTailnet } from "./transport";
import { loadProfile } from "./profile";
import { opusOk, qualityBudget, reportSpeed, reportStall } from "./quality";

function makeAudio() {
  const a = new Audio();
  a.preload = "auto";
  (a as HTMLAudioElement & { playsInline?: boolean }).playsInline = true;
  return a;
}
const els = [makeAudio(), makeAudio()];
/** The element playing the current queue item. */
let audio = els[0];
const spareEl = () => (audio === els[0] ? els[1] : els[0]);

/** Whether this browser lets pages change volume (iOS doesn't): needed for crossfades. */
export const crossfadeSupported = (() => {
  const a = new Audio();
  a.volume = 0.5;
  return Math.abs(a.volume - 0.5) < 0.01;
})();
/** Preload into the spare element and switch on track change (see the header comment). */
const dual = crossfadeSupported;

// ---- crossfade setting (per device) ------------------------------------------------------

const CROSSFADE_KEY = "ytp.crossfade";
export const CROSSFADE_MAX = 12;
function readCrossfade() {
  try {
    const n = Number(localStorage.getItem(CROSSFADE_KEY));
    return isFinite(n) ? Math.max(0, Math.min(CROSSFADE_MAX, n)) : 0;
  } catch {
    return 0;
  }
}
let crossfade = readCrossfade();
export const getCrossfade = () => crossfade;
export function setCrossfade(seconds: number) {
  crossfade = Math.max(0, Math.min(CROSSFADE_MAX, seconds));
  try {
    localStorage.setItem(CROSSFADE_KEY, String(crossfade));
  } catch {
    /* private mode: keep it for this session */
  }
}

// ---- volume (per device) ------------------------------------------------------------------

const VOLUME_KEY = "ytp.volume";
/**
 * Whether this device gets a volume slider. Phones and tablets (touch only) always play at full
 * volume and leave the level to their volume buttons; iOS ignores page volume anyway.
 */
export const volumeAdjustable =
  crossfadeSupported && !(typeof matchMedia === "function" && matchMedia("(hover: none) and (pointer: coarse)").matches);
function readVolume() {
  if (!volumeAdjustable) return 1;
  try {
    const raw = localStorage.getItem(VOLUME_KEY);
    const n = raw == null ? 1 : Number(raw);
    return isFinite(n) ? Math.max(0, Math.min(1, n)) : 1;
  } catch {
    return 1;
  }
}
/** Master volume, 0–1. Element volumes (crossfades included) are scaled by it. */
let volume = readVolume();
for (const el of els) el.volume = volume;
useStore.setState({ volume });
export function setVolume(v: number) {
  if (!volumeAdjustable) return;
  volume = Math.max(0, Math.min(1, v));
  try {
    localStorage.setItem(VOLUME_KEY, String(volume));
  } catch {
    /* private mode: keep it for this session */
  }
  useStore.setState({ volume });
  // A running crossfade picks the new level up on its next step.
  if (!fade) audio.volume = volume;
}

let loadedUid: string | null = null;
/** Track primed synchronously inside a user gesture, before the server confirmed the queue change. */
let pendingVideoId: string | null = null;
let sessionId = "";
let sessionTrack: Track | null = null;
let lastReportAt = 0;
let triedFallback = false;
let loadToken = 0;
let warmTimer: ReturnType<typeof setTimeout> | undefined;
/** Video id the active element has loaded (set once its audio is actually in place). */
let activeVideoId: string | null = null;
/** What's loaded (or loading) in the spare element. `ready` is false while a tailnet download runs. */
let spare: { el: HTMLAudioElement; videoId: string; key: string; ready: boolean; source?: AudioSource | null } | null = null;

// ---- per-element stream info and speed measurement ----------------------------------------

/**
 * Measures how fast an element's stream downloads, as seconds of audio per second of real time,
 * from the moment audio starts arriving (so the server's own YouTube/Plex lookup isn't counted).
 * Times the stream's bitrate, that's the speed of the connection to the server.
 */
type Meter = { start: number; base: number; lastEnd: number; lastT: number; done: boolean; ratio?: number };
type ElInfo = { videoId: string; url: string; kbps: number | null; meter: Meter | null };
const info = new Map<HTMLAudioElement, ElInfo>();

function assign(el: HTMLAudioElement, videoId: string, url: string, kbps: number | null = null) {
  info.set(el, { videoId, url, kbps, meter: null });
}

/** End of the loaded range that starts at the beginning of the track. */
function loadedFromStart(el: HTMLAudioElement) {
  const b = el.buffered;
  for (let i = 0; i < b.length; i++) if (b.start(i) <= 0.5) return b.end(i);
  return 0;
}

function startMeter(el: HTMLAudioElement) {
  const i = info.get(el);
  if (!i || i.url.startsWith("blob:") || isSilentEl(el)) return; // blobs are measured as they download
  const now = performance.now();
  const base = loadedFromStart(el);
  i.meter = { start: now, base, lastEnd: base, lastT: now, done: false };
}

function measure(el: HTMLAudioElement, final: boolean) {
  const i = info.get(el);
  const m = i?.meter;
  if (!i || !m || m.done) return;
  const end = loadedFromStart(el);
  const now = performance.now();
  if (end > m.lastEnd + 0.01) {
    m.lastEnd = end;
    m.lastT = now;
  }
  const full = isFinite(el.duration) && end >= el.duration - 1;
  // On a slow link don't wait for the whole song: 15 s of audio over 5+ s is enough to judge.
  const enough = m.lastEnd - m.base >= 15 && (m.lastT - m.start) / 1000 >= 5;
  if (!full && !final && !enough) return;
  m.done = true;
  const audioSecs = m.lastEnd - m.base;
  const wallSecs = (m.lastT - m.start) / 1000;
  if (audioSecs < 15 || wallSecs < 0.3) return; // too little to judge, or it came from the cache
  m.ratio = audioSecs / wallSecs;
  if (i.kbps) reportSpeed(m.ratio * i.kbps);
}

/** The stream's bitrate arrived (from `/source`): record it, and finish a pending measurement. */
function setKbps(el: HTMLAudioElement, videoId: string, kbps: number | null) {
  const i = info.get(el);
  if (!i || i.videoId !== videoId) return;
  i.kbps = kbps;
  if (kbps && i.meter?.ratio) reportSpeed(i.meter.ratio * kbps);
  if (el === audio) set({ streamKbps: kbps });
}

/** When the server couldn't say (its lookup failed): the usual bitrate for the source and budget. */
function typicalKbps(source: AudioSource): number | null {
  if (source === "cache") return 192;
  if (source !== "youtube") return null;
  const b = qualityBudget();
  // Without Opus, YouTube is always AAC 140 (the server avoids HE-AAC, which Safari mistimes).
  return !opusOk || b == null || b >= 128 ? 130 : b >= 64 ? 66 : 50;
}

/** Browsers that play HLS themselves (Safari). */
const nativeHls = (() => {
  try {
    return new Audio().canPlayType("application/vnd.apple.mpegurl") !== "";
  } catch {
    return false;
  }
})();
/** Songs whose HLS playlist failed this session: they use the plain stream. */
const hlsFailed = new Set<string>();
/** Where the server last said each song plays from (`/source`). */
const knownSource = new Map<string, AudioSource>();

/**
 * Safari plays YouTube songs from an HLS playlist of the file (`hlsUrl`): read as a plain file it
 * misjudges the length (twice the real one, the rest silence) and reads a long song's whole file
 * before starting. It needs to know the source first: other sources play their file directly.
 */
const hlsWanted = () => nativeHls && !opusOk && !isTailnet();

/** How this device plays server songs, and why (Settings → This device). */
export function playbackMode(): string {
  if (isTailnet()) return "through Tailscale: each song downloads completely, then plays";
  if (hlsWanted()) return `YouTube songs as HLS (Safari)${dual ? ", preloading the next song" : ""}`;
  const why = [!nativeHls && "no built-in HLS", opusOk && "plays Opus"].filter(Boolean).join(", ");
  return `plain streams${why ? ` (${why})` : ""}${dual ? ", preloading the next song" : ""}`;
}

/** Ask the server where a song plays from (remembered for `playUrl`). Null: it couldn't say. */
function sourceFirst(id: string): Promise<SourceInfo | null> {
  return api.source(id).then(
    (s) => {
      knownSource.set(id, s.source);
      return s;
    },
    () => null,
  );
}

/** The server URL to play a track from: the plain stream, or in Safari a YouTube song's HLS playlist. */
function playUrl(track: Track): string {
  const id = track.videoId;
  const hls = hlsWanted() && knownSource.get(id) === "youtube" && !hlsFailed.has(id);
  return hls ? hlsUrl(id) : streamUrl(id);
}

function publishBuffered() {
  const b = audio.buffered;
  const out: [number, number][] = [];
  for (let i = 0; i < b.length; i++) out.push([Math.floor(b.start(i) * 2) / 2, Math.ceil(b.end(i) * 2) / 2]);
  const prev = get().buffered;
  if (prev.length === out.length && prev.every((r, i) => r[0] === out[i][0] && r[1] === out[i][1])) return;
  set({ buffered: out });
}

// ---- stalls ---------------------------------------------------------------------------------

/** Waiting right after a load or seek is normal; only count stalls after this time. */
let quietUntil = 0;
let lastQualityReload = -Infinity;
const quiet = () => (quietUntil = performance.now() + 4000);

// Watchdog: a song that should be playing but hasn't moved has stalled: for 2 s mid-song, or for
// 5 s at the start once audio has begun arriving (browsers like Firefox hold a song at 0:00 until
// they've buffered enough to play it through; waiting on the server's own lookup isn't counted).
// Polling rather than the "waiting" event, which browsers fire inconsistently.
let watchTime = -1;
let watchSince = 0;
setInterval(() => {
  const now = performance.now();
  if (audio.paused || audio.ended || !audio.src || isSilent() || isTailnet()) {
    watchTime = -1;
    return;
  }
  if (audio.currentTime !== watchTime) {
    watchTime = audio.currentTime;
    watchSince = now;
    return;
  }
  if (now < quietUntil) return;
  const started = audio.currentTime >= 1;
  let since = watchSince;
  if (!started) {
    const arrived = info.get(audio)?.meter?.start; // set when audio began arriving
    if (arrived == null) return;
    since = Math.max(since, arrived);
  }
  if (now - since > (started ? 2000 : 5000)) {
    watchSince = now; // count each stall once
    onStall();
  }
}, 500);

function onStall() {
  if (audio.paused || isTailnet()) return;
  const lower = reportStall(info.get(audio)?.kbps ?? null);
  // Without Opus, YouTube has only the one AAC file that plays right: a reload gets the same.
  if (!opusOk && get().source === "youtube") return;
  if (!lower || performance.now() - lastQualityReload < 20000) return;
  lastQualityReload = performance.now();
  get().toastMsg("Slow connection to the server: switching to lower quality");
  reloadAtPosition();
}

/** Reload the current song at the current quality budget, carrying on from the same spot. */
function reloadAtPosition(play = true) {
  const it = current();
  if (!it || it.track.videoId !== activeVideoId) return;
  const el = audio;
  const at = el.currentTime;
  const local = localUrl(it.track.videoId);
  const url = local ?? playUrl(it.track);
  el.src = url;
  assign(el, it.track.videoId, url);
  if (local) set({ source: "local", streamKbps: null });
  else fetchSource(el, it.track.videoId, loadToken);
  quiet();
  el.addEventListener("loadedmetadata", () => audio === el && (el.currentTime = at), { once: true });
  if (play) el.play().catch(() => {});
}

/**
 * The server now plays `videoId` from a different source (a Plex version was picked, or Plex
 * turned off for it): forget copies loaded in advance, and if it's the song playing, switch it
 * over at the same spot, keeping it playing or paused.
 */
export function reloadSource(videoId: string) {
  bumpSourceRev(videoId);
  invalidateLocal(videoId);
  for (const [k, v] of blobs) {
    if (!k.startsWith(`${videoId}|`) && !k.startsWith(`${videoId}pipe|`)) continue;
    if (!els.some((el) => el.src === v.url)) URL.revokeObjectURL(v.url);
    blobs.delete(k);
  }
  if (spare?.videoId === videoId) {
    resetEl(spare.el);
    spare = null;
  }
  const it = current();
  if (!it || it.track.videoId !== videoId || activeVideoId !== videoId) return;
  const play = !audio.paused;
  // The music folder on this device may have it now (or not any more): decide again first.
  if (needsDecision(videoId)) {
    void prepareLocal(it.track).then(() => switchSource(videoId, play));
    return;
  }
  switchSource(videoId, play);
}

/** Load `videoId` (the song playing) again from wherever it plays from now, at the same spot. */
function switchSource(videoId: string, play: boolean) {
  const it = current();
  if (!it || it.track.videoId !== videoId || activeVideoId !== videoId) return;
  if (!isTailnet() || localUrl(videoId)) return reloadAtPosition(play);
  const el = audio;
  const at = el.currentTime;
  const old = el.src;
  const token = loadToken;
  loadBlob(videoId)
    .then((l) => {
      if (token !== loadToken || audio !== el) return;
      el.src = l.url;
      assign(el, videoId, l.url, l.kbps);
      set({ source: l.source, streamKbps: l.kbps });
      quiet();
      el.addEventListener("loadedmetadata", () => audio === el && (el.currentTime = at), { once: true });
      if (play) el.play().catch(() => {});
      if (old.startsWith("blob:")) URL.revokeObjectURL(old);
    })
    .catch((e) => get().toastMsg(`Couldn't switch versions: ${(e as Error).message}`));
}

const set = useStore.setState;
const get = useStore.getState;

function current(): QueueItem | undefined {
  const q = get().queue;
  return q ? q.items[q.index] : undefined;
}

// ---- iOS autoplay unlock ---------------------------------------------------------------
// Playing anything from inside a tap "unlocks" the element so later async play() calls work.

let silentUrl: string | null = null;
let unlocked = false;
function silent() {
  if (!silentUrl) {
    const n = 800;
    const b = new ArrayBuffer(44 + n * 2);
    const v = new DataView(b);
    const w = (o: number, s: string) => [...s].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
    w(0, "RIFF"); v.setUint32(4, 36 + n * 2, true); w(8, "WAVE"); w(12, "fmt ");
    v.setUint32(16, 16, true); v.setUint16(20, 1, true); v.setUint16(22, 1, true);
    v.setUint32(24, 8000, true); v.setUint32(28, 16000, true); v.setUint16(32, 2, true); v.setUint16(34, 16, true);
    w(36, "data"); v.setUint32(40, n * 2, true);
    silentUrl = URL.createObjectURL(new Blob([b], { type: "audio/wav" }));
  }
  return silentUrl;
}
const isSilentEl = (el: HTMLAudioElement) => !!silentUrl && el.src === silentUrl;
const isSilent = () => isSilentEl(audio);
function unlockAudio() {
  if (unlocked) return;
  unlocked = true;
  audio.src = silent();
  audio.play().catch(() => {});
}

// ---- tailnet audio blobs -----------------------------------------------------------------

type Loaded = { url: string; source: AudioSource | null; kbps: number | null };
const blobs = new Map<string, Loaded>();
const inflight = new Map<string, Promise<Loaded>>();

function loadBlob(videoId: string, via?: "pipe"): Promise<Loaded> {
  const key = `${videoId}${via ?? ""}|${qualityBudget() ?? "max"}`;
  const hit = blobs.get(key);
  if (hit) {
    blobs.delete(key); // refresh LRU position
    blobs.set(key, hit);
    return Promise.resolve(hit);
  }
  const running = inflight.get(key);
  if (running) return running;
  const p = fetchAudio(videoId, via, undefined, !opusOk)
    .then(async (r) => {
      if (!r.ok) throw new Error(`stream failed (${r.status})`);
      const source = (r.headers.get("x-audio-source") as Loaded["source"]) ?? null;
      const type = r.headers.get("content-type") ?? "audio/mp4";
      const kbps = Number(r.headers.get("x-audio-kbps")) || null;
      // Timed from the response headers, so the server's own lookup isn't counted.
      const t0 = performance.now();
      const data = await r.arrayBuffer();
      const secs = (performance.now() - t0) / 1000;
      if (data.byteLength > 300_000 && secs > 0.2) reportSpeed((data.byteLength * 8) / 1000 / secs);
      const blob = new Blob([data], { type });
      const loaded = { url: URL.createObjectURL(blob), source, kbps };
      blobs.set(key, loaded);
      // Keep a few tracks around; never revoke the one that's playing.
      for (const [k, v] of blobs) {
        if (blobs.size <= 4) break;
        if (!els.some((el) => el.src === v.url)) {
          URL.revokeObjectURL(v.url);
          blobs.delete(k);
        }
      }
      return loaded;
    })
    .finally(() => inflight.delete(key));
  inflight.set(key, p);
  return p;
}

// ---- reporting -------------------------------------------------------------------------

function report(event: "start" | "progress" | "end") {
  if (!sessionTrack || !sessionId) return;
  lastReportAt = Date.now();
  api
    .playback({
      sessionId,
      track: sessionTrack,
      event,
      position: audio.currentTime || 0,
      duration: isFinite(audio.duration) ? audio.duration : undefined,
      source: get().source,
    })
    .catch(() => {});
}

function beginSession(track: Track) {
  if (sessionTrack) report("end");
  sessionId = randomId();
  sessionTrack = track;
  lastReportAt = 0;
}

// ---- loading ---------------------------------------------------------------------------

function setSrc(track: Track, autoplay: boolean, fadeSeconds = 0) {
  triedFallback = false;
  const token = ++loadToken;
  pending = true;
  // `playing`: what it will do once loaded, so the button shows that (with the loading ring).
  set({ source: null, position: 0, duration: track.duration ?? 0, buffering: true, buffered: [], streamKbps: null, playing: autoplay });
  updateMediaSession(track);
  scheduleWarmNext();
  quiet();
  // Whether it plays from the music folder on this device is decided first (only when one is set
  // up and on; the server is asked about the sources ahead of it).
  if (needsDecision(track.videoId)) {
    void prepareLocal(track).then(() => token === loadToken && loadSrc(track, autoplay, fadeSeconds, token));
    return;
  }
  loadSrc(track, autoplay, fadeSeconds, token);
}

function loadSrc(track: Track, autoplay: boolean, fadeSeconds: number, token: number) {
  if (dual) return setSrcDual(track, autoplay, fadeSeconds, token);

  const local = localUrl(track.videoId);
  if (!local && hlsWanted()) return loadAfterSource(track, autoplay, token);
  if (local || !isTailnet()) {
    const url = local ?? playUrl(track);
    audio.src = url;
    assign(audio, track.videoId, url);
    activeVideoId = track.videoId;
    loaded();
    if (autoplay) audio.play().catch(() => {});
    if (local) set({ source: "local", streamKbps: null });
    else fetchSource(audio, track.videoId, token);
    return;
  }

  // The previous song stops while this one downloads.
  audio.pause();
  loadBlob(track.videoId)
    .then((l) => {
      if (token !== loadToken) return;
      lastReportAt = 0;
      audio.src = l.url;
      assign(audio, track.videoId, l.url, l.kbps);
      activeVideoId = track.videoId;
      loaded();
      set({ source: l.source, streamKbps: l.kbps });
      if (autoplay) audio.play().catch(() => {});
      else set({ buffering: false });
    })
    .catch((e) => {
      if (token !== loadToken) return;
      loaded();
      get().toastMsg(`Couldn't load “${track.title}”: ${(e as Error).message}`);
      set({ buffering: false, playing: false });
    });
}

/** Safari: ask where the song plays from, then load the matching URL (see `playUrl`). */
function loadAfterSource(track: Track, autoplay: boolean, token: number) {
  const id = track.videoId;
  audio.pause();
  void sourceFirst(id).then((s) => {
      if (token !== loadToken) return;
      const url = playUrl(track);
      audio.src = url;
      assign(audio, id, url);
      activeVideoId = id;
      loaded();
      if (s) {
        setKbps(audio, id, s.kbps ?? typicalKbps(s.source));
        set({ source: s.source });
      }
      if (autoplay) audio.play().catch(() => {});
    });
}

/** Two-element version: use the preloaded spare when it holds this track, else load into it. */
function setSrcDual(track: Track, autoplay: boolean, fadeSeconds: number, token: number) {
  // A crossfade still running (e.g. skipping again mid-fade): finish it so its element is free.
  if (!fadeSeconds) endFade();
  const el = spareEl();
  // In Safari a spare loaded before its source was known may hold the plain stream of a YouTube song.
  const pre =
    spare && spare.el === el && spare.videoId === track.videoId && spare.ready && !el.error && (localUrl(track.videoId) || spare.key === playUrl(track))
      ? spare
      : null;
  spare = null;
  if (pre) {
    if (pre.source !== undefined) set({ source: pre.source });
    else fetchSource(el, track.videoId, token);
    activate(el, track.videoId, autoplay, fadeSeconds);
    return;
  }
  const local = localUrl(track.videoId);
  if (!local && hlsWanted()) {
    if (!fadeSeconds) audio.pause();
    void sourceFirst(track.videoId).then((s) => {
      if (token !== loadToken) return;
      const url = playUrl(track);
      el.src = url;
      assign(el, track.videoId, url);
      if (s) {
        setKbps(el, track.videoId, s.kbps ?? typicalKbps(s.source));
        set({ source: s.source });
      }
      activate(el, track.videoId, autoplay, fadeSeconds);
    });
    return;
  }
  if (local || !isTailnet()) {
    const url = local ?? playUrl(track);
    el.src = url;
    assign(el, track.videoId, url);
    if (local) set({ source: "local" });
    else fetchSource(el, track.videoId, token);
    activate(el, track.videoId, autoplay, fadeSeconds);
    return;
  }
  // The old song shouldn't keep playing while the new one downloads (unless we're fading).
  if (!fadeSeconds) audio.pause();
  loadBlob(track.videoId)
    .then((l) => {
      if (token !== loadToken) return;
      el.src = l.url;
      assign(el, track.videoId, l.url, l.kbps);
      set({ source: l.source });
      activate(el, track.videoId, autoplay, fadeSeconds);
    })
    .catch((e) => {
      if (token !== loadToken) return;
      loaded();
      get().toastMsg(`Couldn't load “${track.title}”: ${(e as Error).message}`);
      set({ buffering: false, playing: false });
    });
}

function fetchSource(el: HTMLAudioElement, videoId: string, token: number) {
  api
    .source(videoId)
    .then((s) => {
      knownSource.set(videoId, s.source);
      setKbps(el, videoId, s.kbps ?? typicalKbps(s.source));
      if (token === loadToken) set({ source: s.source });
    })
    .catch(() => {});
}

/** Make `el` the playing element; the previous one stops, or fades out over `fadeSeconds`. */
function activate(el: HTMLAudioElement, videoId: string, autoplay: boolean, fadeSeconds: number) {
  const old = audio;
  audio = el;
  loaded();
  activeVideoId = videoId;
  lastReportAt = 0;
  quiet();
  set({ streamKbps: info.get(el)?.kbps ?? null });
  publishBuffered();
  if (fadeSeconds > 0 && old !== el && !old.paused && !isSilentEl(old)) {
    el.volume = 0;
    startFade(old, el, fadeSeconds);
  } else {
    el.volume = volume;
    if (old !== el) {
      old.pause();
      resetEl(old);
    }
  }
  if (autoplay) el.play().catch(() => {});
  else set({ playing: false, buffering: false });
  if (el.readyState >= 3) set({ buffering: false });
  if (isFinite(el.duration)) set({ duration: el.duration });
}

function resetEl(el: HTMLAudioElement) {
  if (!el.getAttribute("src")) return;
  el.removeAttribute("src");
  el.load();
}

// ---- preloading the next song --------------------------------------------------------------

/** The current song is fully downloaded (or nearly over, if the browser stopped buffering ahead). */
function currentLoaded() {
  if (isTailnet()) return true; // a blob is complete by the time it plays
  const d = audio.duration;
  if (!isFinite(d) || d <= 0) return false;
  const b = audio.buffered;
  if (b.length && b.end(b.length - 1) >= d - 1) return true;
  return d - audio.currentTime < 30 + crossfade;
}

function maybePreload() {
  if (!dual || fade) return;
  const q = get().queue;
  const cur = current();
  const n = q?.items[q.index + 1];
  if (!cur || cur.uid !== loadedUid || cur.track.videoId !== activeVideoId || !audio.src || isSilent()) return;
  if (!n) return;
  const videoId = n.track.videoId;
  const el = spareEl();
  if (needsDecision(videoId)) {
    // Decide (music folder or server) once the current song has loaded, then preload.
    if (currentLoaded()) void prepareLocal(n.track).then(() => maybePreload());
    return;
  }
  const local = localUrl(videoId);
  const key = local ?? playUrl(n.track); // includes the quality budget
  if (spare && spare.el === el && spare.videoId === videoId) {
    // Already preloaded. Redo it only if the quality budget changed and it isn't fully loaded yet.
    const full = isFinite(el.duration) && loadedFromStart(el) >= el.duration - 1;
    if (spare.key === key || !spare.ready || full) return;
  }
  if (!currentLoaded()) return;
  if (!local && hlsWanted() && !knownSource.has(videoId)) {
    // Safari: the URL depends on the source (see playUrl); preload once it's known.
    void sourceFirst(videoId).then((s) => s && maybePreload());
    return;
  }
  const entry: NonNullable<typeof spare> = { el, videoId, key, ready: false };
  spare = entry;
  if (local) {
    el.src = local;
    assign(el, videoId, local);
    el.load();
    entry.ready = true;
    entry.source = "local";
    return;
  }
  if (!isTailnet()) {
    el.src = key;
    assign(el, videoId, key);
    el.load();
    entry.ready = true;
    api
      .source(videoId)
      .then((s) => {
        entry.source = s.source;
        setKbps(el, videoId, s.kbps ?? typicalKbps(s.source));
      })
      .catch(() => {});
    return;
  }
  loadBlob(videoId)
    .then((l) => {
      if (spare !== entry || el === audio) return;
      el.src = l.url;
      assign(el, videoId, l.url, l.kbps);
      el.load();
      entry.source = l.source;
      entry.ready = true;
    })
    .catch(() => spare === entry && (spare = null));
}

// ---- crossfade ----------------------------------------------------------------------------

let fade: { old: HTMLAudioElement; neu: HTMLAudioElement; timer: ReturnType<typeof setInterval> } | null = null;

function startFade(old: HTMLAudioElement, neu: HTMLAudioElement, seconds: number) {
  endFade();
  const ms = seconds * 1000;
  // The clock starts once the new song is actually playing, so a slow start doesn't cut the old one short.
  let start: number | null = null;
  const step = () => {
    if (start === null) {
      if (neu.paused || neu.currentTime <= 0) return;
      start = performance.now();
    }
    const t = Math.min(1, (performance.now() - start) / ms);
    // Equal-power curve: the overall loudness stays even through the fade.
    neu.volume = volume * Math.sin((t * Math.PI) / 2);
    old.volume = volume * Math.cos((t * Math.PI) / 2);
    if (t >= 1 || old.ended) endFade();
  };
  fade = { old, neu, timer: setInterval(step, 50) };
}

function endFade() {
  if (!fade) return;
  const { old, neu, timer } = fade;
  fade = null;
  clearInterval(timer);
  neu.volume = volume;
  old.pause();
  old.volume = volume;
  resetEl(old);
  maybePreload();
}

/** Start the next song early and fade across, `crossfade` seconds before the end. */
function checkCrossfade() {
  if (!dual || crossfade <= 0 || fade || audio.paused || isSilent()) return;
  const d = audio.duration;
  if (!isFinite(d) || d < crossfade * 2 + 5) return; // short tracks just play out
  const remaining = d - audio.currentTime;
  if (remaining > crossfade || remaining <= 0.2) return;
  const q = get().queue;
  const cur = current();
  if (!q || !cur || cur.track.videoId !== activeVideoId || !q.items[q.index + 1]) return;
  next(true, remaining);
}

/** Call inside a click handler so mobile browsers allow playback to start. */
function prime(track: Track, fadeSeconds = 0) {
  pendingVideoId = track.videoId;
  if (isTailnet()) unlockAudio();
  beginSession(track);
  setSrc(track, true, fadeSeconds);
}

let followedProfile: string | undefined;

function follow(q: Queue | null) {
  // Switching listener profile swaps the whole queue: stop rather than jump into the other profile's track.
  const profileChanged = !!q && followedProfile !== undefined && q.profile !== followedProfile;
  if (q) followedProfile = q.profile;
  if (profileChanged) audio.pause();
  const item = q ? q.items[q.index] : undefined;
  if (!item) {
    if (loadedUid) {
      endFade();
      audio.pause();
      audio.removeAttribute("src");
      audio.load();
      if (spare) resetEl(spare.el);
      spare = null;
      activeVideoId = null;
      loadedUid = null;
      if (sessionTrack) report("end");
      sessionTrack = null;
      set({ playing: false, position: 0, duration: 0, source: null, buffered: [], streamKbps: null });
    }
    return;
  }
  if (item.uid === loadedUid) return;
  if (pendingVideoId && item.track.videoId === pendingVideoId) {
    // Already loading it from the user gesture.
    pendingVideoId = null;
    loadedUid = item.uid;
    return;
  }
  const wasPlaying = !audio.paused && !profileChanged;
  loadedUid = item.uid;
  beginSession(item.track);
  setSrc(item.track, wasPlaying);
}

useStore.subscribe((s, prev) => {
  if (s.queue !== prev.queue) {
    follow(s.queue);
    // The next song may have changed (queue edited, shuffled, added to).
    maybePreload();
  }
});

/** Get the next track ready ahead of time so transitions are near-gapless. */
function scheduleWarmNext() {
  clearTimeout(warmTimer);
  if (dual) return; // the spare element preloads instead (maybePreload)
  warmTimer = setTimeout(
    () => {
      const q = get().queue;
      const next = q?.items[q.index + 1];
      if (!next) return;
      if (needsDecision(next.track.videoId)) void prepareLocal(next.track);
      if (localUrl(next.track.videoId)) return;
      if (isTailnet()) {
        loadBlob(next.track.videoId).catch(() => {});
      } else {
        // Makes the server resolve the stream URL with yt-dlp now rather than at transition time.
        fetch(streamUrl(next.track.videoId), { headers: { Range: "bytes=0-1" } })
          .then((r) => r.body?.cancel())
          .catch(() => {});
      }
    },
    isTailnet() ? 3000 : 8000,
  );
}

// ---- audio events ------------------------------------------------------------------------
// Both elements get the listeners; only the active one drives the UI and the queue. The spare's
// (preloading) and a fading-out element's events are ignored, except a failed preload.

/**
 * A new song is on its way (looking up its source, or downloading it through the tailnet) and the
 * element still holds the previous one: its events (time, length, pause, ending) aren't the new
 * song's, so they're ignored until the new song is in place (`loaded`).
 */
let pending = false;
/** The new song is in the playing element: its events count again. */
function loaded() {
  pending = false;
}

function on(type: string, fn: (el: HTMLAudioElement) => void) {
  for (const el of els) {
    el.addEventListener(type, () => {
      if (el === audio && !isSilentEl(el) && !pending) fn(el);
    });
  }
}

for (const el of els) {
  // Speed measurement runs on both elements (the spare's preload counts too).
  el.addEventListener("loadedmetadata", () => startMeter(el));
  el.addEventListener("progress", () => measure(el, false));
  el.addEventListener("suspend", () => measure(el, true));
  el.addEventListener("seeking", () => {
    const m = info.get(el)?.meter;
    if (m) m.done = true;
  });
  el.addEventListener("error", () => {
    if (spare && spare.el === el && el !== audio) {
      spare = null; // load it normally when its turn comes
    }
  });
}

on("playing", () => {
  set({ playing: true, buffering: false });
  if (lastReportAt === 0) report("start");
});
on("pause", () => set({ playing: false }));
on("waiting", () => set({ buffering: true }));
on("canplay", () => set({ buffering: false }));
on("durationchange", () => {
  if (isFinite(audio.duration)) set({ duration: audio.duration });
  publishBuffered();
});
on("progress", () => {
  publishBuffered();
  maybePreload();
});
on("timeupdate", () => {
  set({ position: audio.currentTime });
  publishBuffered();
  if (!audio.paused && Date.now() - lastReportAt > 15000) report("progress");
  maybePreload();
  checkCrossfade();
  if ("mediaSession" in navigator && isFinite(audio.duration) && audio.duration > 0) {
    try {
      navigator.mediaSession.setPositionState({
        duration: audio.duration,
        position: Math.min(audio.currentTime, audio.duration),
        playbackRate: audio.playbackRate,
      });
    } catch {
      /* ignore */
    }
  }
});
on("ended", () => {
  report("end");
  sessionTrack = null;
  next(true);
});
on("error", () => {
  const it = current();
  if (!it || !audio.src) return;
  const local = localUrl(it.track.videoId);
  if (local && audio.src === local) {
    // The file in the music folder couldn't be played (moved, unreadable): the server plays it.
    skipLocal(it.track.videoId);
    setSrc(it.track, true);
    return;
  }
  if (audio.src.includes("/hls")) {
    // The HLS playlist didn't play: the plain stream, from the same spot.
    hlsFailed.add(it.track.videoId);
    const at = audio.currentTime;
    const url = streamUrl(it.track.videoId);
    audio.src = url;
    assign(audio, it.track.videoId, url);
    if (at > 0) audio.addEventListener("loadedmetadata", () => (audio.currentTime = at), { once: true });
    audio.play().catch(() => {});
    return;
  }
  if (!triedFallback) {
    // Retry through yt-dlp's pipe mode (no seeking, but robust).
    triedFallback = true;
    const token = loadToken;
    if (isTailnet()) {
      loadBlob(it.track.videoId, "pipe")
        .then((l) => {
          if (token !== loadToken) return;
          audio.src = l.url;
          assign(audio, it.track.videoId, l.url, l.kbps);
          audio.play().catch(() => {});
        })
        .catch(() => next(true));
    } else {
      // The page's address may have stopped reaching the server (e.g. left home Wi-Fi):
      // re-check, and reload this track over the Tailscale fallback if that's now in use.
      const at = audio.currentTime;
      detectConnection(true).then((c) => {
        if (token !== loadToken) return;
        if (c?.mode === "tailnet") {
          setSrc(it.track, true);
          if (at > 0) audio.addEventListener("loadedmetadata", () => (audio.currentTime = at), { once: true });
          return;
        }
        const url = streamUrl(it.track.videoId, "pipe");
        audio.src = url;
        assign(audio, it.track.videoId, url);
        audio.play().catch(() => {});
      });
    }
    return;
  }
  get().toastMsg(`Couldn't play “${it.track.title}”, skipping`);
  next(true);
});

// ---- media session (lock screen / headphones / car) ---------------------------------------

function updateMediaSession(t: Track) {
  if (!("mediaSession" in navigator)) return;
  navigator.mediaSession.metadata = new MediaMetadata({
    title: t.title,
    artist: t.artist,
    album: t.album ?? "",
    artwork: t.thumbnail ? [{ src: t.thumbnail, sizes: "544x544", type: "image/jpeg" }] : [],
  });
}

if ("mediaSession" in navigator) {
  const ms = navigator.mediaSession;
  ms.setActionHandler("play", () => play());
  ms.setActionHandler("pause", () => pause());
  ms.setActionHandler("previoustrack", () => previous());
  ms.setActionHandler("nexttrack", () => next(true));
  ms.setActionHandler("seekto", (d) => d.seekTime != null && seek(d.seekTime));
  ms.setActionHandler("seekbackward", (d) => seek(audio.currentTime - (d.seekOffset ?? 10)));
  ms.setActionHandler("seekforward", (d) => seek(audio.currentTime + (d.seekOffset ?? 10)));
}

// ---- public controls ------------------------------------------------------------------------

async function apply(p: Promise<Queue>) {
  try {
    get().setQueue(await p);
  } catch (e) {
    pendingVideoId = null;
    get().toastMsg((e as Error).message);
  }
}

export function play() {
  const it = current();
  if (!it) return;
  if (isTailnet()) unlockAudio();
  if (it.uid !== loadedUid || isSilent() || !audio.src) {
    loadedUid = it.uid;
    beginSession(it.track);
    setSrc(it.track, true);
    return;
  }
  audio.play().catch(() => {});
}

export function pause() {
  endFade();
  audio.pause();
}

export function toggle() {
  if (audio.paused) play();
  else pause();
}

export function seek(t: number) {
  if (!isFinite(t)) return;
  endFade();
  quiet();
  audio.currentTime = Math.max(0, Math.min(t, isFinite(audio.duration) ? audio.duration - 0.5 : t));
}

/** `fadeSeconds`: crossfade into it (used near the end of a song when crossfade is on). */
export function next(auto = false, fadeSeconds = 0) {
  const q = get().queue;
  if (!q) return;
  const n = q.items[q.index + 1];
  if (!n) {
    if (!auto) get().toastMsg("End of queue");
    audio.pause();
    return;
  }
  prime(n.track, fadeSeconds);
  apply(api.queue.setIndex(q.index + 1));
}

export function previous() {
  const q = get().queue;
  if (!q) return;
  if (audio.currentTime > 4 || q.index === 0) {
    seek(0);
    return;
  }
  prime(q.items[q.index - 1].track);
  apply(api.queue.setIndex(q.index - 1));
}

export function jumpTo(index: number) {
  const q = get().queue;
  const it = q?.items[index];
  if (!it) return;
  prime(it.track);
  apply(api.queue.setIndex(index));
}

/** Replace the queue with `tracks` and start at `index`. */
export function playTracks(tracks: Track[], index = 0) {
  if (!tracks.length) return;
  prime(tracks[index]);
  apply(api.queue.replace(tracks, index));
}

/** Play a single track followed by an automatic radio (YouTube Music + Last.fm). */
export function playRadio(track: Track) {
  prime(track);
  apply(api.queue.radio(track));
}

export function enqueue(tracks: Track[], at: InsertAt = "end") {
  if (!tracks.length) return;
  const wasEmpty = !get().queue?.items.length;
  if (at === "now" || wasEmpty) prime(tracks[0]);
  apply(api.queue.add(tracks, at));
  if (at === "next") get().toastMsg(tracks.length > 1 ? `${tracks.length} tracks will play next` : "Playing next");
  if (at === "end" && !wasEmpty) get().toastMsg(tracks.length > 1 ? `Added ${tracks.length} tracks to queue` : "Added to queue");
}

export function queueOp(p: Promise<Queue>) {
  return apply(p);
}

// ---- sync with server --------------------------------------------------------------------------

/** Server-sent events over apiFetch, so it works through the in-page tailnet too. */
async function eventLoop() {
  for (;;) {
    try {
      const res = await apiFetch("/events", { headers: { accept: "text/event-stream" } });
      if (!res.ok || !res.body) throw new Error(`events: ${res.status}`);
      // Songs with no audio: the full list on (re)connecting, then changes as events.
      api
        .missing()
        .then((ids) => set({ missing: new Set(ids) }))
        .catch(() => {});
      const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
      let buf = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += value;
        let idx;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          const data = block
            .split("\n")
            .filter((l) => l.startsWith("data:"))
            .map((l) => l.slice(5).trimStart())
            .join("\n");
          if (!data) continue;
          try {
            const ev = JSON.parse(data);
            if (ev.type === "queue") get().setQueue(ev.queue);
            if (ev.type === "import") set((s) => ({ importTick: s.importTick + 1 }));
            if (ev.type === "profile") loadProfile();
            if (ev.type === "missing")
              set((s) => {
                const m = new Set(s.missing);
                if (ev.missing) m.add(ev.videoId);
                else m.delete(ev.videoId);
                return { missing: m };
              });
          } catch {
            /* ignore */
          }
        }
      }
    } catch {
      /* reconnect below */
    }
    await new Promise((r) => setTimeout(r, 3000));
  }
}

let started = false;

export async function initPlayer() {
  if (started) return;
  started = true;
  try {
    const q = await api.queue.get();
    // Don't autoplay on load; just show where we left off.
    const it = q.items[q.index];
    if (it) {
      loadedUid = it.uid;
      beginSession(it.track);
      setSrc(it.track, false);
      set({ buffering: false });
    }
    get().setQueue(q);
  } catch {
    /* unauthorized or offline; UI handles it */
  }
  eventLoop();
  // Re-sync whenever the app comes back to the foreground (streams may have been suspended).
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") api.queue.get().then(get().setQueue).catch(() => {});
  });
}
