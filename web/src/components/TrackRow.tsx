import { useState, type ReactNode } from "react";
import { fmtTime, type Track } from "../lib/api";
import { currentTrack, useStore } from "../lib/store";
import { usePressMenu } from "../lib/pressMenu";
import { isTailnet } from "../lib/transport";
import { useLocalArt } from "../lib/localFolder";
import { MoreIcon } from "./Icons";

/**
 * Artwork through the server's cache (downloaded once, then served locally). Over the
 * in-page tailnet an <img> can't use the tunnel, so the original URL is used there.
 */
export function artUrl(src?: string | null) {
  if (!src) return src;
  if (isTailnet() || !/^https:\/\/[^/]*(googleusercontent\.com|ggpht\.com|ytimg\.com)\//.test(src)) return src;
  return `/api/art?u=${encodeURIComponent(src)}`;
}

/**
 * Artwork. With the song's `videoId`, a red ⊗ shows on it while nothing can play the song (no
 * Plex copy, no YouTube match, or the video is gone).
 */
export function Cover({
  src,
  size = 48,
  round = false,
  videoId,
}: {
  src?: string | null;
  size?: number;
  round?: boolean;
  videoId?: string | null;
}) {
  const missing = useStore((s) => (videoId ? s.missing.has(videoId) : false));
  // A song from the music folder on this device: its cover is in the folder's index.
  const localArt = useLocalArt(src ? null : videoId);
  src = src || localArt;
  // Artwork that failed to load (a library track without a cover answers 404): the placeholder.
  const [broken, setBroken] = useState<string | null>(null);
  return (
    <div className={`cover ${round ? "round" : ""}`} style={{ width: size, height: size }}>
      {src && src !== broken ? (
        <img src={artUrl(src) ?? undefined} alt="" loading="lazy" referrerPolicy="no-referrer" onError={() => setBroken(src)} />
      ) : (
        <span>♪</span>
      )}
      {missing && (
        <span className="cover-missing" title="No audio found for this song. Pick one with ⋯ → Change audio source." role="img" aria-label="No audio">
          <svg viewBox="0 0 24 24" width={Math.max(16, Math.min(44, size * 0.42))} height={Math.max(16, Math.min(44, size * 0.42))} aria-hidden>
            <circle cx="12" cy="12" r="10.5" fill="#e5383b" stroke="#fff" strokeWidth="1.5" />
            <path d="M8 8l8 8M16 8l-8 8" stroke="#fff" strokeWidth="2.4" strokeLinecap="round" />
          </svg>
        </span>
      )}
    </div>
  );
}

type Props = {
  track: Track;
  onPlay: () => void;
  active?: boolean;
  leading?: ReactNode;
  trailing?: ReactNode;
  dim?: boolean;
};

export function TrackRow({ track, onPlay, active, leading, trailing, dim }: Props) {
  const openMenu = useStore((s) => s.openMenu);
  const playingId = useStore((s) => currentTrack(s)?.videoId);
  const isActive = active ?? playingId === track.videoId;
  const press = usePressMenu(() => openMenu(track));
  return (
    <div className={`track-row ${isActive ? "active" : ""} ${dim ? "dim" : ""}`}>
      {leading}
      <button className="track-main" onClick={onPlay} {...press}>
        <Cover src={track.thumbnail} videoId={track.videoId} />
        <div className="track-text">
          <div className="track-title">{track.title}</div>
          <div className="track-sub">
            {track.artist}
            {track.album ? ` · ${track.album}` : ""}
          </div>
        </div>
      </button>
      <span className="track-dur">{fmtTime(track.duration)}</span>
      {trailing}
      <button className="icon-btn" aria-label="More" onClick={() => openMenu(track)}>
        <MoreIcon />
      </button>
    </div>
  );
}

export function TrackList({ tracks, onPlay }: { tracks: Track[]; onPlay: (index: number) => void }) {
  return (
    <div className="track-list">
      {tracks.map((t, i) => (
        <TrackRow key={`${t.videoId}-${i}`} track={t} onPlay={() => onPlay(i)} />
      ))}
    </div>
  );
}
