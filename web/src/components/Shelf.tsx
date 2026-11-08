import { Link, useNavigate } from "react-router-dom";
import type { HomeItem, Shelf } from "../lib/api";
import { playRadio, playTracks } from "../lib/player";
import { usePressMenu } from "../lib/pressMenu";
import { useStore } from "../lib/store";
import { PlayIcon } from "./Icons";
import { Cover } from "./TrackRow";

/** Where a card leads: artist page, album/playlist, or (for songs) nowhere — songs play. */
export function itemLink(item: HomeItem): string | null {
  if (item.kind === "artist") {
    return item.browseId ? `/artist/${encodeURIComponent(item.browseId)}` : `/artist?name=${encodeURIComponent(item.title)}`;
  }
  const id = item.playlistId ?? item.browseId;
  if (!item.track && id) return `/collection/${encodeURIComponent(id)}?title=${encodeURIComponent(item.title)}`;
  return null;
}

/** "Go to artist" for a song, which only knows the artist's name. */
export function artistLink(artist: string) {
  const first = artist.split(/,| & | feat\. | ft\. /i)[0].trim();
  return `/artist?name=${encodeURIComponent(first)}`;
}

export function Card({ item, size = 148, onPlay }: { item: HomeItem; size?: number; onPlay?: () => void }) {
  const nav = useNavigate();
  const openMenu = useStore((s) => s.openMenu);
  const link = itemLink(item);
  const track = item.track;
  const press = usePressMenu(track ? () => openMenu(track) : null);
  return (
    <div className="card" style={{ width: size }}>
      <button
        className="card-btn"
        onClick={() => {
          if (item.track) (onPlay ?? (() => playRadio(item.track!)))();
          else if (link) nav(link);
        }}
        {...press}
      >
        <Cover src={item.thumbnail} size={size} round={item.kind === "artist"} videoId={item.track?.videoId ?? item.videoId} />
        <div className={`card-title ${item.kind === "artist" ? "center" : ""}`}>{item.title}</div>
        {item.subtitle && <div className={`card-sub ${item.kind === "artist" ? "center" : ""}`}>{item.subtitle}</div>}
      </button>
    </div>
  );
}

/** A titled, horizontally scrolling row of cards. */
export function ShelfView({ shelf, more }: { shelf: Shelf; more?: string | null }) {
  const songs = shelf.items.filter((i) => i.track);
  const allSongs = songs.length === shelf.items.length;
  // Quick-pick style shelves (all songs) play in order; single songs elsewhere start a radio.
  const playFrom = (item: HomeItem) =>
    allSongs && songs.length > 1
      ? () =>
          playTracks(
            songs.map((s) => s.track!),
            songs.indexOf(item),
          )
      : undefined;
  return (
    <section className="shelf">
      <div className="shelf-head">
        <h2>{shelf.title}</h2>
        <div className="row gap">
          {allSongs && songs.length > 1 && (
            <button className="btn small ghost" onClick={() => playTracks(songs.map((s) => s.track!))}>
              <PlayIcon size={14} /> Play all
            </button>
          )}
          {more && (
            <Link className="btn small ghost" to={more}>
              See all
            </Link>
          )}
        </div>
      </div>
      <div className="shelf-scroll">
        {shelf.items.map((item, i) => (
          <Card key={i} item={item} onPlay={playFrom(item)} />
        ))}
      </div>
    </section>
  );
}
