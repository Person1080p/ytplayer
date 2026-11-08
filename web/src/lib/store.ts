import { create } from "zustand";
import type { AudioSource, Queue, Track } from "./api";

type State = {
  queue: Queue | null;
  playing: boolean;
  buffering: boolean;
  position: number;
  duration: number;
  source: AudioSource | null;
  /** Loaded parts of the current track, in seconds. */
  buffered: [number, number][];
  /** Bitrate (kbps) of the current track's stream, when known. */
  streamKbps: number | null;
  /** Bitrate budget new streams are requested with (null: no limit). See quality.ts. */
  qualityBudget: number | null;
  /** Measured speed of the connection to the server (kbps), once measured. */
  netKbps: number | null;
  /** Master volume on this device, 0–1 (always 1 on phones and tablets). See player.ts. */
  volume: number;
  nowPlayingOpen: boolean;
  menuTrack: Track | null;
  playlistPickerTracks: Track[] | null;
  /** Song whose Plex version is being chosen (see PlexVersionSheet). */
  plexMatchTrack: Track | null;
  toast: { id: number; text: string } | null;
  importTick: number;
  /** Songs nothing can play right now (no Plex copy, no YouTube match, or the video is gone). */
  missing: Set<string>;

  setQueue: (q: Queue) => void;
  toastMsg: (text: string) => void;
  openMenu: (t: Track | null) => void;
  pickPlaylist: (tracks: Track[] | null) => void;
  choosePlexVersion: (t: Track | null) => void;
  setNowPlaying: (open: boolean) => void;
};

export const useStore = create<State>((set, get) => ({
  queue: null,
  playing: false,
  buffering: false,
  position: 0,
  duration: 0,
  source: null,
  buffered: [],
  streamKbps: null,
  qualityBudget: null,
  netKbps: null,
  volume: 1,
  nowPlayingOpen: false,
  menuTrack: null,
  playlistPickerTracks: null,
  plexMatchTrack: null,
  toast: null,
  importTick: 0,
  missing: new Set(),

  setQueue: (q) => {
    const cur = get().queue;
    // Responses and SSE events can arrive out of order; keep the newest.
    // (The server bumps the version on profile switches too.)
    if (cur && q.version < cur.version) return;
    set({ queue: q });
  },
  toastMsg: (text) => {
    const id = Date.now();
    set({ toast: { id, text } });
    setTimeout(() => {
      if (get().toast?.id === id) set({ toast: null });
    }, 2800);
  },
  openMenu: (t) => set({ menuTrack: t }),
  pickPlaylist: (tracks) => set({ playlistPickerTracks: tracks, menuTrack: null }),
  choosePlexVersion: (t) => set({ plexMatchTrack: t, menuTrack: null }),
  setNowPlaying: (open) => set({ nowPlayingOpen: open }),
}));

export const currentTrack = (s: State) => (s.queue ? s.queue.items[s.queue.index]?.track ?? null : null);
