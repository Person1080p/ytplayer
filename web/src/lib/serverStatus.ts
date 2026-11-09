// The server's status (`/status`), shared by the pages and sheets that need to know which audio
// sources are set up and on. Fetched once when the app connects, and again whenever a page asks
// for it fresh (Settings, Library), so changes made in Settings show everywhere.

import { useEffect } from "react";
import { create } from "zustand";
import { api, type LibrarySourceId, type SourceId, type Status } from "./api";
import { useMusicFolder } from "./localFolder";

export const useServerStatus = create<{ status: Status | null }>(() => ({ status: null }));

let inflight: Promise<Status> | null = null;

/** Fetch `/status` now and share it. Rejects like `api.status()`. */
export function fetchStatus(): Promise<Status> {
  inflight ??= api.status().then(
    (s) => {
      inflight = null;
      useServerStatus.setState({ status: s });
      return s;
    },
    (e) => {
      inflight = null;
      throw e;
    },
  );
  return inflight;
}

/** The shared status, fetched first if nothing has loaded yet. */
export function loadStatus(): Promise<Status | null> {
  const s = useServerStatus.getState().status;
  return s ? Promise.resolve(s) : fetchStatus().catch(() => null);
}

/** Whether a source is on in Settings → Audio sources (on when the server doesn't say). */
export function sourceOn(s: Status | null | undefined, id: SourceId): boolean {
  return s?.prefs.sources?.find((p) => p.id === id)?.on ?? true;
}

/** YouTube is on (also while the status hasn't loaded, so nothing changes for most setups). */
export const youtubeOn = (s: Status | null | undefined) => sourceOn(s, "youtube");

/** Whether a library source is set up: Plex or Jellyfin connected, or a music folder readable on
 *  this device. */
export function sourceConnected(s: Status | null | undefined, id: LibrarySourceId): boolean {
  if (id === "local") return useMusicFolder.getState().state === "ready";
  if (!s) return false;
  if (id === "plex") return s.plex.connected;
  return !!s.jellyfin?.connected;
}

/** React hook: the shared status (loaded once if nothing has yet). */
export function useStatus(): Status | null {
  const status = useServerStatus((st) => st.status);
  useEffect(() => {
    if (!status) void loadStatus();
  }, [status]);
  return status;
}

/** React hook: whether YouTube is on. */
export const useYoutubeOn = () => youtubeOn(useStatus());
