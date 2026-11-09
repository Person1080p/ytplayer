// The music folder on this device: an audio source the web app handles itself (the server can't
// see it). The folder and all its subfolders are scanned first, into an index kept in this
// browser (IndexedDB): each file's tags (or what its name and folders say), length and cover.
// Matching only reads that index; a matched song plays straight from the file.
//
// Chrome and Edge on a computer can keep the folder between visits (the File System Access API;
// the browser asks again for permission now and then). Other browsers pick the folder for the
// visit only (`<input webkitdirectory>`): the index stays, the files need picking again.
//
// Songs found only in the folder use `local_<16 hex>` ids made from their artist and title, so
// the same song gets the same id on every device. The server treats them like imported songs.

import { useEffect, useState } from "react";
import { create } from "zustand";
import { api, isLocalId, registerLocalLibrary, type LibraryApi, type LibraryChoice, type SourceId, type Track } from "./api";
import { normalize, primaryArtist, rank, looksLive } from "./matching";
import { sourceConnected, sourceOn, useServerStatus } from "./serverStatus";

/** Automatic matches below this score aren't used (same scale as the server's libraries). */
const MATCH_THRESHOLD = 0.82;
const AUDIO = /\.(mp3|m4a|aac|flac|ogg|oga|opus|wav|webm)$/i;
const COVER = /^(cover|folder|front|album|albumart[^.]*)\.(jpe?g|png|webp)$/i;
const DISC_DIR = /^(cd|dis[ck])\s*\d+$/i;

// ---- minimal File System Access typings (not in TypeScript's DOM lib yet) ---------------------

type PermissionMode = { mode: "read" };
interface DirHandle {
  kind: "directory";
  name: string;
  values(): AsyncIterable<DirHandle | FileHandle>;
  getDirectoryHandle(name: string): Promise<DirHandle>;
  getFileHandle(name: string): Promise<FileHandle>;
  queryPermission?(d: PermissionMode): Promise<PermissionState>;
  requestPermission?(d: PermissionMode): Promise<PermissionState>;
}
interface FileHandle {
  kind: "file";
  name: string;
  getFile(): Promise<File>;
}
type PickerWindow = Window & { showDirectoryPicker?: (o?: { id?: string; mode?: "read" }) => Promise<DirHandle> };

/** This browser can keep a folder between visits. */
export const canKeepFolder = typeof window !== "undefined" && "showDirectoryPicker" in window;
/** This browser can pick a folder at all (for the visit at least). */
export const canPickFolder =
  canKeepFolder || (typeof document !== "undefined" && "webkitdirectory" in document.createElement("input"));

// ---- state --------------------------------------------------------------------------------

/** One audio file in the index. `path` is relative to the chosen folder, with `/`. */
type FileRow = {
  path: string;
  id: string;
  title: string;
  artist: string;
  albumArtist: string | null;
  album: string | null;
  duration: number | null;
  codec: string | null;
  size: number;
  mtime: number;
  normTitle: string;
  normArtist: string;
  /** "embedded" (stored in the `art` store), a cover image's path, or null. */
  cover: string | null;
};

type Pick = { mode: "picked"; id: string } | { mode: "off" };

export type FolderState = {
  /** none: no folder chosen; ready: files readable; permission: the browser needs a click to
   *  allow reading the folder again; repick: the folder has to be picked again (this browser
   *  can't keep it between visits). */
  state: "none" | "ready" | "permission" | "repick";
  name: string | null;
  tracks: number;
  scanning: boolean;
  /** Files looked at so far in the running scan. */
  scanned: number;
  lastScanAt: number | null;
  error: string | null;
};

export const useMusicFolder = create<FolderState>(() => ({
  state: "none",
  name: null,
  tracks: 0,
  scanning: false,
  scanned: 0,
  lastScanAt: null,
  error: null,
}));
const setState = useMusicFolder.setState;

let handle: DirHandle | null = null;
/** Files picked for this visit (browsers that can't keep a folder). */
let picked: Map<string, File> | null = null;
let rows: FileRow[] = [];
const byPath = new Map<string, FileRow>();
const byId = new Map<string, FileRow>();
const picks = new Map<string, Pick>();

function setRows(list: FileRow[]) {
  rows = list;
  byPath.clear();
  byId.clear();
  for (const r of list) {
    byPath.set(r.path, r);
    if (!byId.has(r.id)) byId.set(r.id, r);
  }
  setState({ tracks: list.length });
}

// ---- IndexedDB ------------------------------------------------------------------------------

let dbp: Promise<IDBDatabase> | null = null;
function db(): Promise<IDBDatabase> {
  dbp ??= new Promise((resolve, reject) => {
    const req = indexedDB.open("ytp-music-folder", 1);
    req.onupgradeneeded = () => {
      const d = req.result;
      d.createObjectStore("meta");
      d.createObjectStore("files", { keyPath: "path" });
      d.createObjectStore("art");
      d.createObjectStore("picks");
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
  return dbp;
}

function done(t: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    t.oncomplete = () => resolve();
    t.onerror = () => reject(t.error);
    t.onabort = () => reject(t.error);
  });
}

function request<T>(r: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

async function setMeta(key: string, value: unknown) {
  const d = await db();
  const t = d.transaction("meta", "readwrite");
  t.objectStore("meta").put(value, key);
  await done(t);
}

// ---- start-up -------------------------------------------------------------------------------

async function init() {
  try {
    const d = await db();
    const t = d.transaction(["files", "picks", "meta"]);
    const [list, pickKeys, pickVals, h, name, last] = await Promise.all([
      request(t.objectStore("files").getAll()) as Promise<FileRow[]>,
      request(t.objectStore("picks").getAllKeys()),
      request(t.objectStore("picks").getAll()) as Promise<Pick[]>,
      request(t.objectStore("meta").get("handle")) as Promise<DirHandle | undefined>,
      request(t.objectStore("meta").get("name")) as Promise<string | undefined>,
      request(t.objectStore("meta").get("lastScanAt")) as Promise<number | undefined>,
    ]);
    setRows(list);
    pickKeys.forEach((k, i) => picks.set(String(k), pickVals[i]));
    setState({ name: name ?? null, lastScanAt: last ?? null });
    if (h) {
      handle = h;
      const p = await h.queryPermission?.({ mode: "read" });
      setState({ state: p === "granted" ? "ready" : "permission" });
      // Catch up with files added or removed since the last visit.
      if (p === "granted") void rescanFolder();
    } else if (name) {
      setState({ state: "repick" });
    }
  } catch (e) {
    setState({ error: `This browser can't keep a music folder index (${(e as Error).message})` });
  }
}
void init();

// ---- choosing the folder --------------------------------------------------------------------

/** Choose the folder (call from a click). Scans it right away. */
export async function pickFolder(): Promise<void> {
  const w = window as PickerWindow;
  if (w.showDirectoryPicker) {
    let h: DirHandle;
    try {
      h = await w.showDirectoryPicker({ id: "ytp-music", mode: "read" });
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      throw e;
    }
    await switchFolder(h.name);
    handle = h;
    picked = null;
    await setMeta("handle", h);
    setState({ state: "ready" });
    await rescanFolder();
    return;
  }
  // Pick for this visit only.
  const files = await pickWithInput();
  if (!files) return;
  const name = files.root;
  if (name !== useMusicFolder.getState().name) await switchFolder(name);
  handle = null;
  picked = files.map;
  await setMeta("name", name);
  setState({ state: "ready", name });
  await rescanFolder();
}

function pickWithInput(): Promise<{ root: string; map: Map<string, File> } | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    (input as HTMLInputElement & { webkitdirectory: boolean }).webkitdirectory = true;
    input.addEventListener("change", () => {
      const list = [...(input.files ?? [])];
      if (!list.length) return resolve(null);
      const map = new Map<string, File>();
      let root = "";
      for (const f of list) {
        // "Music/Artist/Album/01 Song.mp3": drop the chosen folder's own name.
        const rel = (f as File & { webkitRelativePath: string }).webkitRelativePath || f.name;
        const [first, ...rest] = rel.split("/");
        root ||= first;
        map.set(rest.length ? rest.join("/") : first, f);
      }
      resolve({ root, map });
    });
    input.addEventListener("cancel", () => resolve(null));
    input.click();
  });
}

/** A different folder than before: start its index afresh. */
async function switchFolder(name: string) {
  const d = await db();
  const t = d.transaction(["files", "art", "meta"], "readwrite");
  t.objectStore("files").clear();
  t.objectStore("art").clear();
  t.objectStore("meta").put(name, "name");
  t.objectStore("meta").delete("lastScanAt");
  await done(t);
  setRows([]);
  forgetDecisions();
  setState({ name, lastScanAt: null });
}

/** Let the browser read the folder again (call from a click). */
export async function reconnectFolder(): Promise<void> {
  if (!handle?.requestPermission) return;
  const p = await handle.requestPermission({ mode: "read" });
  if (p !== "granted") return;
  setState({ state: "ready" });
  forgetDecisions();
  await rescanFolder();
}

/** Stop using the folder on this device: its index and picks are removed. */
export async function forgetFolder(): Promise<void> {
  const d = await db();
  const t = d.transaction(["files", "art", "meta", "picks"], "readwrite");
  for (const s of ["files", "art", "meta", "picks"]) t.objectStore(s).clear();
  await done(t);
  handle = null;
  picked = null;
  picks.clear();
  setRows([]);
  forgetDecisions();
  setState({ state: "none", name: null, lastScanAt: null, error: null, scanned: 0 });
}

// ---- scanning -------------------------------------------------------------------------------

type Found = { path: string; file: File; cover: string | null };

/** Every audio file under the folder, with the cover image of its folder (if any). */
async function* walk(): AsyncGenerator<Found> {
  if (handle) {
    yield* walkDir(handle, "", null);
    return;
  }
  if (!picked) return;
  // Picked files: cover images per folder first.
  const covers = new Map<string, string>();
  for (const p of picked.keys()) {
    const slash = p.lastIndexOf("/");
    if (COVER.test(p.slice(slash + 1))) covers.set(p.slice(0, Math.max(0, slash)), p);
  }
  for (const [path, file] of picked) {
    if (!AUDIO.test(path) || hidden(path)) continue;
    const dir = path.slice(0, Math.max(0, path.lastIndexOf("/")));
    const parent = DISC_DIR.test(dir.split("/").pop() ?? "") ? dir.slice(0, Math.max(0, dir.lastIndexOf("/"))) : null;
    yield { path, file, cover: covers.get(dir) ?? (parent != null ? covers.get(parent) ?? null : null) };
  }
}

const hidden = (path: string) => path.split("/").some((s) => s.startsWith(".") || s === "$RECYCLE.BIN" || s === "System Volume Information");

async function* walkDir(dir: DirHandle, prefix: string, parentCover: string | null): AsyncGenerator<Found> {
  const files: FileHandle[] = [];
  const dirs: DirHandle[] = [];
  for await (const e of dir.values()) {
    if (e.name.startsWith(".") || e.name === "$RECYCLE.BIN" || e.name === "System Volume Information") continue;
    if (e.kind === "file") files.push(e);
    else dirs.push(e);
  }
  const coverFile = files.find((f) => COVER.test(f.name));
  const cover = coverFile ? prefix + coverFile.name : DISC_DIR.test(dir.name) ? parentCover : null;
  for (const f of files) {
    if (!AUDIO.test(f.name)) continue;
    try {
      yield { path: prefix + f.name, file: await f.getFile(), cover };
    } catch {
      /* unreadable file: skip */
    }
  }
  for (const d of dirs) yield* walkDir(d, `${prefix}${d.name}/`, cover);
}

let scanning: Promise<void> | null = null;

/** Scan the folder again: new and changed files are read, removed ones dropped. */
export function rescanFolder(): Promise<void> {
  if (useMusicFolder.getState().state !== "ready") return Promise.resolve();
  scanning ??= scan().finally(() => (scanning = null));
  return scanning;
}

async function scan() {
  setState({ scanning: true, scanned: 0, error: null });
  let count = 0;
  const seen = new Set<string>();
  let batch: { row: FileRow; art: Blob | null }[] = [];
  const flush = async () => {
    if (!batch.length) return;
    const d = await db();
    const t = d.transaction(["files", "art"], "readwrite");
    for (const { row, art } of batch) {
      t.objectStore("files").put(row);
      if (art) t.objectStore("art").put(art, row.path);
      else t.objectStore("art").delete(row.path);
    }
    await done(t);
    const next = new Map(byPath);
    for (const { row } of batch) next.set(row.path, row);
    setRows([...next.values()]);
    batch = [];
  };
  try {
    for await (const f of walk()) {
      seen.add(f.path);
      if (++count % 25 === 0) setState({ scanned: count });
      const old = byPath.get(f.path);
      if (old && old.size === f.file.size && old.mtime === f.file.lastModified) {
        // Unchanged file; only a cover image added to or removed from its folder is picked up.
        if (old.cover !== "embedded" && old.cover !== f.cover) batch.push({ row: { ...old, cover: f.cover }, art: null });
        continue;
      }
      batch.push(await readFile(f));
      if (batch.length >= 100) await flush();
    }
    await flush();
    // Files gone from the folder.
    const gone = rows.filter((r) => !seen.has(r.path));
    if (gone.length) {
      const d = await db();
      const t = d.transaction(["files", "art"], "readwrite");
      for (const r of gone) {
        t.objectStore("files").delete(r.path);
        t.objectStore("art").delete(r.path);
      }
      await done(t);
      setRows(rows.filter((r) => seen.has(r.path)));
    }
    const at = Math.floor(Date.now() / 1000);
    await setMeta("lastScanAt", at);
    setState({ lastScanAt: at, scanned: count });
  } catch (e) {
    const err = e as Error;
    // The folder was moved or deleted, or permission was taken back.
    if (err.name === "NotAllowedError" || err.name === "SecurityError") setState({ state: "permission" });
    setState({ error: `Scan stopped: ${err.message}` });
  } finally {
    forgetDecisions();
    artUrls.clear();
    setState({ scanning: false });
  }
}

/** A file's index row: its tags, else what its name and folders say. */
async function readFile({ path, file, cover }: Found): Promise<{ row: FileRow; art: Blob | null }> {
  const guess = fromPath(path);
  let title = guess.title;
  let artist = guess.artist ?? "";
  let album = guess.album;
  let albumArtist: string | null = null;
  let duration: number | null = null;
  let codec: string | null = null;
  let art: Blob | null = null;
  try {
    const { parseBlob } = await import("music-metadata");
    const m = await parseBlob(file, { duration: false, skipCovers: false });
    const c = m.common;
    title = c.title?.trim() || title;
    artist = c.artist?.trim() || c.artists?.join(", ") || c.albumartist?.trim() || artist;
    album = c.album?.trim() || album;
    albumArtist = c.albumartist?.trim() || null;
    duration = m.format.duration ? Math.round(m.format.duration) : null;
    codec = m.format.codec?.toLowerCase() ?? null;
    const pic = c.picture?.find((p) => /front|cover/i.test(p.type ?? "")) ?? c.picture?.[0];
    if (pic) art = new Blob([pic.data as BlobPart], { type: pic.format || "image/jpeg" });
  } catch {
    /* no readable tags: the name and folders it is */
  }
  artist ||= "Unknown artist";
  const row: FileRow = {
    path,
    id: songId(artist, title),
    title,
    artist,
    albumArtist,
    album,
    duration,
    codec,
    size: file.size,
    mtime: file.lastModified,
    normTitle: normalize(title),
    normArtist: primaryArtist(artist),
    cover: art ? "embedded" : cover,
  };
  return { row, art };
}

/** Title, artist and album from a file's name and folders ("Artist/Album (2001)/01 - Title.mp3",
 *  "Artist - Title.flac"), for files without tags. */
export function fromPath(path: string): { title: string; artist: string | null; album: string | null } {
  const parts = path.split("/");
  const base = parts[parts.length - 1].replace(/\.[^.]+$/, "").replaceAll("_", " ").trim();
  let dirs = parts.slice(0, -1);
  if (dirs.length && DISC_DIR.test(dirs[dirs.length - 1])) dirs = dirs.slice(0, -1);
  const album = dirs.length ? cleanAlbum(dirs[dirs.length - 1]) : null;
  const folderArtist = dirs.length >= 2 ? dirs[dirs.length - 2] : null;
  const dash = (s: string) => s.match(/^(.+?)\s+-\s+(.+)$/);
  // "Artist - Title" (also "50 Cent - Title": a number that's part of the name stays).
  let m = dash(base);
  if (m && !/^\d{1,3}([-.]\d{1,3})?$/.test(m[1].trim())) return { title: m[2].trim(), artist: m[1].trim(), album };
  // Leading track number: "01 - Title", "01. Title", "1-02 Title", "07 Title".
  const rest = base.replace(/^\d{1,3}([-.]\d{1,3})?(\s*-\s*|\.\s*|\s+)/, "").trim() || base;
  m = dash(rest);
  if (m) return { title: m[2].trim(), artist: m[1].trim(), album };
  return { title: rest, artist: folderArtist, album };
}

function cleanAlbum(s: string): string {
  return s.replace(/\s*[([]\d{4}[)\]]\s*$/, "").replace(/^\d{4}\s*-\s*/, "").trim() || s;
}

/** `local_<16 hex>`: the same id for the same song on every device (FNV-1a of artist + title). */
export function songId(artist: string, title: string): string {
  const key = `${primaryArtist(artist)}|${normalize(title)}`;
  let h = 0xcbf29ce484222325n;
  for (const b of new TextEncoder().encode(key)) {
    h ^= BigInt(b);
    h = (h * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return `local_${h.toString(16).padStart(16, "0")}`;
}

// ---- reading files --------------------------------------------------------------------------

async function fileAt(path: string): Promise<File | null> {
  if (picked) return picked.get(path) ?? null;
  if (!handle) return null;
  try {
    const parts = path.split("/");
    let dir = handle;
    for (const p of parts.slice(0, -1)) dir = await dir.getDirectoryHandle(p);
    return await (await dir.getFileHandle(parts[parts.length - 1])).getFile();
  } catch {
    return null;
  }
}

function toTrack(r: FileRow): Track {
  return { videoId: r.id, title: r.title, artist: r.artist, album: r.album, duration: r.duration, thumbnail: null };
}

// ---- matching -------------------------------------------------------------------------------

type Want = { videoId: string; title: string; artist: string; album?: string | null; duration?: number | null };

/** Files that could be `want`, best first, with their match score. */
function candidates(want: Want): [FileRow, number][] {
  const wt = normalize(want.title);
  if (!wt) return [];
  const found = rows.filter(
    (r) => r.normTitle === wt || (wt.length >= 3 && r.normTitle.length >= 3 && (r.normTitle.includes(wt) || wt.includes(r.normTitle))),
  );
  return found
    .map((r) => {
      const [score, order] = rank(want, r);
      return { r, score, order };
    })
    .sort((a, b) => b.order - a.order)
    .map(({ r, score }) => [r, score]);
}

/** The file a song plays from in the folder (a pick of yours wins), or null. */
function fileFor(want: Want): FileRow | null {
  if (isLocalId(want.videoId)) return byId.get(want.videoId) ?? null;
  const p = picks.get(want.videoId);
  if (p?.mode === "off") return null;
  if (p?.mode === "picked") return byId.get(p.id) ?? null;
  const best = candidates(want)[0];
  return best && best[1] >= MATCH_THRESHOLD ? best[0] : null;
}

async function savePick(videoId: string, p: Pick | null) {
  const d = await db();
  const t = d.transaction("picks", "readwrite");
  if (p) t.objectStore("picks").put(p, videoId);
  else t.objectStore("picks").delete(videoId);
  await done(t);
  if (p) picks.set(videoId, p);
  else picks.delete(videoId);
  forgetDecision(videoId);
}

/** A few random songs from the folder (for the home page). */
export function randomFolderTracks(n: number): Track[] {
  const seen = new Set<string>();
  const out: Track[] = [];
  const pool = [...rows];
  while (out.length < n && pool.length) {
    const r = pool.splice(Math.floor(Math.random() * pool.length), 1)[0];
    if (!seen.has(r.id)) {
      seen.add(r.id);
      out.push(toTrack(r));
    }
  }
  return out;
}

/** The folder as a library source, with the same calls the server's libraries have. */
export const localApi: LibraryApi = {
  async search(q: string) {
    const words = normalize(q).split(" ").filter(Boolean);
    if (!words.length) return [];
    const nq = normalize(q);
    const hits = rows.filter((r) => {
      const hay = `${r.normTitle} ${normalize(r.artist)} ${normalize(r.albumArtist ?? "")} ${normalize(r.album ?? "")}`;
      return words.every((w) => hay.includes(w));
    });
    const score = (r: FileRow) => (r.normTitle === nq ? 3 : r.normTitle.startsWith(nq) ? 2 : r.normTitle.includes(nq) ? 1 : 0);
    const seen = new Set<string>();
    return hits
      .sort((a, b) => score(b) - score(a) || a.title.localeCompare(b.title))
      .filter((r) => !seen.has(r.id) && seen.add(r.id))
      .slice(0, 30)
      .map(toTrack);
  },

  async match(videoId: string): Promise<LibraryChoice> {
    const want: Want | null = isLocalId(videoId) ? (byId.get(videoId) ? { ...toTrack(byId.get(videoId)!) } : null) : await api.track(videoId);
    if (!want) return { mode: "auto", current: null, candidates: [] };
    const p = picks.get(videoId);
    const found = candidates(want);
    const mode = p?.mode ?? "auto";
    const current =
      p?.mode === "off" ? null : p?.mode === "picked" ? (byId.has(p.id) ? p.id : null) : found.find(([, s]) => s >= MATCH_THRESHOLD)?.[0].id ?? null;
    const seen = new Set<string>();
    return {
      mode,
      current: current?.slice("local_".length) ?? null,
      candidates: found
        .filter(([r, s]) => s >= 0.5 && !seen.has(r.id) && seen.add(r.id))
        .slice(0, 30)
        .map(([r, score]) => ({ track: toTrack(r), score, live: looksLive(r.title, r.album) })),
    };
  },

  async setMatch(videoId: string, itemId: string) {
    const id = itemId.startsWith("local_") ? itemId : `local_${itemId}`;
    if (!byId.has(id)) throw new Error("That file isn't in the music folder any more");
    await savePick(videoId, { mode: "picked", id });
    return {};
  },

  async clearMatch(videoId: string, mode: "off" | "auto") {
    await savePick(videoId, mode === "off" ? { mode: "off" } : null);
    return { ok: true };
  },
};

registerLocalLibrary(localApi);

// ---- which songs play from the folder -------------------------------------------------------

/** Decided songs: the file's object URL, or null (the server plays it). */
const decisions = new Map<string, string | null>();
const deciding = new Map<string, Promise<void>>();

function remember(videoId: string, url: string | null) {
  decisions.delete(videoId);
  decisions.set(videoId, url);
  // Keep the last few dozen; a file URL costs nothing but a handle.
  for (const [k, v] of decisions) {
    if (decisions.size <= 40) break;
    decisions.delete(k);
    if (v) setTimeout(() => URL.revokeObjectURL(v), 60_000);
  }
}

function forgetDecision(videoId: string) {
  const v = decisions.get(videoId);
  decisions.delete(videoId);
  if (v) setTimeout(() => URL.revokeObjectURL(v), 60_000);
}

function forgetDecisions() {
  for (const k of [...decisions.keys()]) forgetDecision(k);
}

/** The folder is ready on this device and on in Settings → Audio sources. */
export function folderActive(): boolean {
  return useMusicFolder.getState().state === "ready" && sourceOn(useServerStatus.getState().status, "local");
}

/** The file URL to play `videoId` from, once decided (null: the server plays it). */
export function localUrl(videoId: string): string | null {
  return folderActive() ? decisions.get(videoId) ?? null : null;
}

/** Whether `prepareLocal` still has to look at this song before it loads. */
export function needsDecision(videoId: string): boolean {
  return folderActive() && !decisions.has(videoId);
}

/**
 * Decide whether a song plays from the folder. In the priority order, sources ahead of the folder
 * are asked first (the server says whether they have it); then the folder's match; else the
 * server plays it from the sources after the folder.
 */
export function prepareLocal(track: Track): Promise<void> {
  const id = track.videoId;
  if (!needsDecision(id)) return Promise.resolve();
  const running = deciding.get(id);
  if (running) return running;
  const p = (async () => {
    let url: string | null = null;
    try {
      const status = useServerStatus.getState().status;
      const order: SourceId[] = (status?.prefs.sources ?? []).filter((s) => s.on).map((s) => s.id);
      const at = order.indexOf("local");
      const ahead = (at < 0 ? [] : order.slice(0, at)).filter((s) => s === "youtube" || sourceConnected(status, s));
      let serverHasIt = false;
      if (!isLocalId(id) && ahead.length) {
        serverHasIt = await api.source(id, { only: ahead }).then(
          () => true,
          () => false,
        );
      }
      if (!serverHasIt) {
        const row = fileFor(track);
        const file = row && (await fileAt(row.path));
        if (file) url = URL.createObjectURL(file);
      }
    } catch {
      /* the server plays it */
    }
    remember(id, url);
  })().finally(() => deciding.delete(id));
  deciding.set(id, p);
  return p;
}

/** The song's file couldn't be played (moved, unreadable): let the server play it this time. */
export function skipLocal(videoId: string) {
  forgetDecision(videoId);
  remember(videoId, null);
}

/** Forget what was decided for a song (or all songs), e.g. after a different copy was picked. */
export function invalidateLocal(videoId?: string) {
  if (videoId) forgetDecision(videoId);
  else forgetDecisions();
}

// Changes to the priority order change which source comes first.
useServerStatus.subscribe((s, prev) => {
  if (JSON.stringify(s.status?.prefs.sources) !== JSON.stringify(prev.status?.prefs.sources)) forgetDecisions();
});

// ---- artwork --------------------------------------------------------------------------------

const artUrls = new Map<string, Promise<string | null>>();

/** Cover art of a song found in the folder (`local_…` id), as an object URL. */
function artFor(videoId: string): Promise<string | null> {
  let p = artUrls.get(videoId);
  if (!p) {
    p = (async () => {
      const r = byId.get(videoId);
      if (!r?.cover) return null;
      let blob: Blob | null = null;
      if (r.cover === "embedded") {
        const d = await db();
        blob = ((await request(d.transaction("art").objectStore("art").get(r.path))) as Blob | undefined) ?? null;
      } else {
        blob = await fileAt(r.cover);
      }
      return blob ? URL.createObjectURL(blob) : null;
    })().catch(() => null);
    artUrls.set(videoId, p);
  }
  return p;
}

/** React hook: the folder's cover for a `local_…` song (null for other songs or none). */
export function useLocalArt(videoId: string | null | undefined): string | null {
  const [url, setUrl] = useState<string | null>(null);
  // Embedded covers are in the index, so they show even before the folder is readable again.
  const ready = useMusicFolder((s) => s.tracks > 0);
  useEffect(() => {
    setUrl(null);
    if (!videoId || !isLocalId(videoId) || !ready) return;
    let live = true;
    void artFor(videoId).then((u) => live && setUrl(u));
    return () => {
      live = false;
    };
  }, [videoId, ready]);
  return url;
}
