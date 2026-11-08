import { useNavigate } from "react-router-dom";
import { useEffect, useRef, useState } from "react";
import { api, fmtTime, type AudioSource, type Track } from "../lib/api";
import { setLiked, useLibrary } from "../lib/library";
import { next, playRadio, previous, seek, setVolume, toggle, volumeAdjustable } from "../lib/player";
import { currentTrack, useStore } from "../lib/store";
import {
  ChevronDown,
  HeartFilledIcon,
  HeartIcon,
  MoreIcon,
  NextIcon,
  PauseIcon,
  PlayIcon,
  PrevIcon,
  QueueIcon,
  RadioIcon,
  VolumeIcon,
  VolumeLowIcon,
  VolumeMuteIcon,
} from "./Icons";
import { artUrl, Cover } from "./TrackRow";
import { artistLink } from "./Shelf";

/** ♥: filled when the track is in your library or loved on Last.fm; toggles both. */
export function HeartButton({ track, size = 22 }: { track: Track; size?: number }) {
  const inLibrary = useLibrary((s) => s.ids.has(track.videoId));
  const [lastfmLoved, setLastfmLoved] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    setLastfmLoved(null);
    let alive = true;
    api
      .liked(track.videoId)
      .then((st) => alive && setLastfmLoved(st.lastfmLoved ?? null))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [track.videoId]);
  const liked = inLibrary || lastfmLoved === true;
  return (
    <button
      className={`icon-btn heart ${liked ? "on" : ""}`}
      aria-label={liked ? "Remove from library" : "Add to library"}
      aria-pressed={liked}
      disabled={busy}
      onClick={async () => {
        setBusy(true);
        const st = await setLiked(track, !liked);
        if (st) setLastfmLoved(st.lastfmLoved ?? null);
        setBusy(false);
      }}
    >
      {liked ? <HeartFilledIcon size={size} /> : <HeartIcon size={size} />}
    </button>
  );
}

const SOURCE_NAMES: Record<AudioSource, string> = { plex: "Plex", cache: "Cached", youtube: "YouTube" };

function SourceBadge() {
  const source = useStore((s) => s.source);
  const kbps = useStore((s) => s.streamKbps);
  const budget = useStore((s) => s.qualityBudget);
  if (!source) return null;
  const name = SOURCE_NAMES[source];
  const tip = [source === "cache" ? "MP3 copy the server kept from a recent YouTube play" : "", kbps ? `${kbps} kbps` : ""]
    .filter(Boolean)
    .join(" · ");
  // Show the bitrate when quality is being limited for the connection.
  return (
    <span className={`badge ${source}`} title={tip || undefined}>
      {name}
      {budget != null && kbps ? ` · ${kbps} kbps` : ""}
    </span>
  );
}

/** Loaded parts of the track, drawn in gray behind the progress. */
function BufferedRanges({ duration }: { duration: number }) {
  const buffered = useStore((s) => s.buffered);
  if (!duration) return null;
  return (
    <>
      {buffered.map(([a, b], i) => (
        <div
          key={i}
          className="seek-buffered"
          style={{ left: `${(Math.min(a, duration) / duration) * 100}%`, width: `${((Math.min(b, duration) - Math.min(a, duration)) / duration) * 100}%` }}
        />
      ))}
    </>
  );
}

/** Devices with a mouse or trackpad (hover works). */
const POINTER_QUERY = "(hover: hover) and (pointer: fine)";

function useMedia(query: string) {
  const [matches, setMatches] = useState(() => typeof matchMedia === "function" && matchMedia(query).matches);
  useEffect(() => {
    const m = matchMedia(query);
    const onChange = () => setMatches(m.matches);
    onChange();
    m.addEventListener("change", onChange);
    return () => m.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

/**
 * Seek bar. `big`: the Now Playing one, with times. Otherwise the player bar's progress line,
 * which is a seek bar too where there's a mouse (on touch screens, tapping the bar opens Now
 * Playing). With a mouse, hovering shows the time under the pointer.
 */
function Seekbar({ big = false }: { big?: boolean }) {
  const position = useStore((s) => s.position);
  const duration = useStore((s) => s.duration);
  // While dragging, show where you are without seeking on every move (seek on release).
  const [drag, setDrag] = useState<number | null>(null);
  const [hover, setHover] = useState<number | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const mouse = useMedia(POINTER_QUERY);
  const shown = drag ?? position;
  const pctOf = (t: number) => (duration ? Math.min(100, (t / duration) * 100) : 0);
  const pct = pctOf(shown);
  if (!big && !mouse) {
    return (
      <div className="mini-progress">
        <BufferedRanges duration={duration} />
        <div className="mini-progress-fill" style={{ width: `${pct}%` }} />
      </div>
    );
  }
  const timeAt = (clientX: number) => {
    const r = ref.current!.getBoundingClientRect();
    return Math.max(0, Math.min(1, (clientX - r.left) / r.width)) * duration;
  };
  const tip = drag ?? hover;
  const hit = (
    <div
      ref={ref}
      className={`seek-hit ${big ? "" : "bar"} ${drag != null ? "dragging" : ""}`}
      role="slider"
      tabIndex={0}
      aria-label="Seek"
      aria-valuemin={0}
      aria-valuemax={Math.round(duration)}
      aria-valuenow={Math.round(shown)}
      aria-valuetext={`${fmtTime(shown)} of ${fmtTime(duration)}`}
      onPointerDown={(e) => {
        if (!duration || e.button !== 0) return;
        e.preventDefault(); // no text selection, no focus jump
        e.currentTarget.setPointerCapture(e.pointerId);
        setDrag(timeAt(e.clientX));
      }}
      onPointerMove={(e) => {
        if (drag != null) setDrag(timeAt(e.clientX));
        else if (e.pointerType === "mouse" && duration) setHover(timeAt(e.clientX));
      }}
      onPointerLeave={() => setHover(null)}
      onPointerUp={(e) => {
        if (drag == null) return;
        seek(timeAt(e.clientX));
        setDrag(null);
      }}
      onPointerCancel={() => setDrag(null)}
      onKeyDown={(e) => {
        const step = { ArrowLeft: -5, ArrowRight: 5, ArrowDown: -10, ArrowUp: 10 }[e.key];
        if (step) seek(position + step);
        else if (e.key === "Home") seek(0);
        else if (e.key === "End") seek(duration);
        else return;
        e.preventDefault();
      }}
    >
      <div className="seek-track">
        <BufferedRanges duration={duration} />
        {hover != null && drag == null && <div className="seek-hover" style={{ width: `${pctOf(hover)}%` }} />}
        <div className="seek-fill" style={{ width: `${pct}%` }} />
        <div className="seek-thumb" style={{ left: `${pct}%` }} />
      </div>
      {tip != null && (
        <div className="seek-tip" style={{ left: `clamp(20px, ${pctOf(tip)}%, calc(100% - 20px))` }}>
          {fmtTime(tip)}
        </div>
      )}
    </div>
  );
  if (!big) return <div className="bar-seek">{hit}</div>;
  return (
    <div className="seek">
      {hit}
      <div className="seek-times">
        <span>{fmtTime(shown)}</span>
        <span>{fmtTime(duration)}</span>
      </div>
    </div>
  );
}

/** Volume slider with a mute button. Not shown on phones and tablets, which always play at full volume. */
function VolumeControl({ className = "" }: { className?: string }) {
  const volume = useStore((s) => s.volume);
  // Level to go back to when unmuting.
  const before = useRef(1);
  if (!volumeAdjustable) return null;
  const Icon = volume === 0 ? VolumeMuteIcon : volume < 0.5 ? VolumeLowIcon : VolumeIcon;
  const pct = Math.round(volume * 100);
  return (
    <div className={`volume ${className}`}>
      <button
        className="icon-btn"
        aria-label={volume === 0 ? "Unmute" : "Mute"}
        title={volume === 0 ? "Unmute" : "Mute"}
        onClick={() => {
          if (volume > 0) {
            before.current = volume;
            setVolume(0);
          } else {
            setVolume(before.current || 1);
          }
        }}
      >
        <Icon size={20} />
      </button>
      <input
        type="range"
        className="volume-range"
        min={0}
        max={1}
        step={0.01}
        value={volume}
        aria-label="Volume"
        aria-valuetext={`${pct}%`}
        title={`Volume ${pct}%`}
        style={{ "--vol": `${pct}%` } as React.CSSProperties}
        onChange={(e) => setVolume(Number(e.target.value))}
        onWheel={(e) => setVolume(volume - Math.sign(e.deltaY) * 0.05)}
      />
    </div>
  );
}

export function PlayerBar() {
  const track = useStore(currentTrack);
  const playing = useStore((s) => s.playing);
  const buffering = useStore((s) => s.buffering);
  const open = useStore((s) => s.setNowPlaying);
  if (!track) return null;
  return (
    <div className="player-bar">
      <Seekbar />
      <div className="player-bar-inner">
        <button className="player-bar-info" onClick={() => open(true)}>
          <Cover src={track.thumbnail} size={44} videoId={track.videoId} />
          <div className="track-text">
            <div className="track-title">{track.title}</div>
            <div className="track-sub">
              {track.artist} <SourceBadge />
            </div>
          </div>
        </button>
        <div className="player-bar-controls">
          <span className="hide-mobile">
            <HeartButton track={track} size={20} />
          </span>
          <button className="icon-btn hide-mobile" onClick={previous} aria-label="Previous">
            <PrevIcon />
          </button>
          <button className={`icon-btn play ${buffering && playing ? "loading" : ""}`} onClick={toggle} aria-label="Play/pause">
            {playing ? <PauseIcon /> : <PlayIcon />}
          </button>
          <button className="icon-btn" onClick={() => next()} aria-label="Next">
            <NextIcon />
          </button>
          <span className="hide-mobile">
            <VolumeControl />
          </span>
        </div>
      </div>
    </div>
  );
}

export function NowPlaying() {
  const isOpen = useStore((s) => s.nowPlayingOpen);
  const setOpen = useStore((s) => s.setNowPlaying);
  const track = useStore(currentTrack);
  const playing = useStore((s) => s.playing);
  const buffering = useStore((s) => s.buffering);
  const openMenu = useStore((s) => s.openMenu);
  const nav = useNavigate();
  if (!isOpen || !track) return null;

  return (
    <div className="now-playing">
      <div
        className="np-bg"
        style={track.thumbnail ? ({ "--np-art": `url("${artUrl(track.thumbnail)}")` } as React.CSSProperties) : undefined}
      />
      <header className="np-header">
        <button className="icon-btn" onClick={() => setOpen(false)} aria-label="Close">
          <ChevronDown size={28} />
        </button>
        <span className="np-label">Now playing</span>
        <button className="icon-btn" onClick={() => openMenu(track)} aria-label="More">
          <MoreIcon />
        </button>
      </header>
      <div className="np-art">
        <Cover src={track.thumbnail} size={340} videoId={track.videoId} />
      </div>
      <div className="np-meta">
        <h2>{track.title}</h2>
        <p>
          <button
            className="link-btn"
            onClick={() => {
              setOpen(false);
              nav(artistLink(track.artist));
            }}
            title="Go to artist"
          >
            {track.artist}
          </button>
          {track.album ? ` · ${track.album}` : ""}
        </p>
        <SourceBadge />
      </div>
      <Seekbar big />
      <div className="np-controls">
        <HeartButton track={track} />
        <button className="icon-btn" onClick={previous} aria-label="Previous">
          <PrevIcon size={30} />
        </button>
        <button className={`icon-btn play big ${buffering && playing ? "loading" : ""}`} onClick={toggle} aria-label="Play/pause">
          {playing ? <PauseIcon size={34} /> : <PlayIcon size={34} />}
        </button>
        <button className="icon-btn" onClick={() => next()} aria-label="Next">
          <NextIcon size={30} />
        </button>
        <button
          className="icon-btn"
          aria-label="Queue"
          onClick={() => {
            setOpen(false);
            nav("/queue");
          }}
        >
          <QueueIcon />
        </button>
      </div>
      <VolumeControl className="np-volume" />
      <button className="btn ghost np-radio" onClick={() => playRadio(track)}>
        <RadioIcon size={18} /> Start radio from this song
      </button>
    </div>
  );
}
