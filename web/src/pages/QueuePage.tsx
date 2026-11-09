import { useEffect, useRef, useState } from "react";
import { api, SOURCE_LABELS, type AudioSource, type HistoryEntry } from "../lib/api";
import { jumpTo, playRadio, queueOp } from "../lib/player";
import { useStore } from "../lib/store";
import { CloseIcon, DragIcon, ListIcon, ShuffleIcon, TrashIcon } from "../components/Icons";
import { TrackRow } from "../components/TrackRow";

/** Pointer-based drag reordering that works with touch and mouse. */
function useDragSort(onMove: (from: number, to: number) => void) {
  const [drag, setDrag] = useState<{ from: number; over: number } | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const start = (index: number) => (e: React.PointerEvent) => {
    e.preventDefault();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    setDrag({ from: index, over: index });
  };
  const move = (e: React.PointerEvent) => {
    if (!drag || !listRef.current) return;
    const rows = Array.from(listRef.current.querySelectorAll<HTMLElement>("[data-qi]"));
    let over = drag.over;
    for (const r of rows) {
      const rect = r.getBoundingClientRect();
      if (e.clientY >= rect.top && e.clientY <= rect.bottom) over = Number(r.dataset.qi);
    }
    if (over !== drag.over) setDrag({ ...drag, over });
  };
  const end = () => {
    if (drag && drag.from !== drag.over) onMove(drag.from, drag.over);
    setDrag(null);
  };
  return { drag, listRef, start, move, end };
}

export default function QueuePage() {
  const queue = useStore((s) => s.queue);
  const pick = useStore((s) => s.pickPlaylist);
  const [tab, setTab] = useState<"queue" | "history">("queue");
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const { drag, listRef, start, move, end } = useDragSort((from, to) => queueOp(api.queue.move(from, to)));

  useEffect(() => {
    if (tab === "history") api.history().then(setHistory).catch(() => {});
  }, [tab]);

  const items = queue?.items ?? [];
  const idx = queue?.index ?? 0;

  return (
    <div className="page">
      <div className="row between">
        <h1>Queue</h1>
        <div className="tabs">
          <button className={tab === "queue" ? "on" : ""} onClick={() => setTab("queue")}>
            Up next
          </button>
          <button className={tab === "history" ? "on" : ""} onClick={() => setTab("history")}>
            History
          </button>
        </div>
      </div>

      {tab === "queue" && (
        <>
          <div className="row gap wrap">
            <label className="switch">
              <input
                type="checkbox"
                checked={queue?.autoplay ?? true}
                onChange={(e) => queueOp(api.queue.autoplay(e.target.checked))}
              />
              <span>Autoplay recommendations</span>
            </label>
            <button className="btn small ghost" onClick={() => queueOp(api.queue.shuffle())} disabled={items.length - idx < 3}>
              <ShuffleIcon size={14} /> Shuffle
            </button>
            <button className="btn small ghost" onClick={() => pick(items.map((i) => i.track))} disabled={!items.length}>
              <ListIcon size={14} /> Save as playlist
            </button>
            <button className="btn small ghost" onClick={() => queueOp(api.queue.clear(false))} disabled={items.length <= idx + 1}>
              <TrashIcon size={14} /> Clear upcoming
            </button>
          </div>

          {items.length === 0 && <p className="muted">The queue is empty. Search for something or pick a mix on Home.</p>}

          <div ref={listRef} className="track-list" onPointerMove={move} onPointerUp={end} onPointerCancel={end}>
            {items.map((it, i) => {
              const past = i < idx;
              const isOver = drag && drag.over === i && drag.from !== i;
              return (
                <div
                  key={it.uid}
                  data-qi={i}
                  className={`queue-item ${drag?.from === i ? "dragging" : ""} ${isOver ? (drag!.from < i ? "over-below" : "over-above") : ""}`}
                >
                  {i === idx + 1 && <div className="queue-sep">Up next</div>}
                  {it.auto && (i === 0 || !items[i - 1].auto) && i > idx && <div className="queue-sep">Autoplay · recommended</div>}
                  <TrackRow
                    track={it.track}
                    active={i === idx}
                    dim={past}
                    onPlay={() => jumpTo(i)}
                    leading={
                      <span className="drag-handle" onPointerDown={start(i)} aria-label="Reorder">
                        <DragIcon size={18} />
                      </span>
                    }
                    trailing={
                      i !== idx && (
                        <button className="icon-btn" aria-label="Remove" onClick={() => queueOp(api.queue.remove(it.uid))}>
                          <CloseIcon size={18} />
                        </button>
                      )
                    }
                  />
                </div>
              );
            })}
          </div>
        </>
      )}

      {tab === "history" && (
        <div className="track-list">
          {history.map((h, i) => (
            <TrackRow
              key={i}
              track={h.track}
              onPlay={() => playRadio(h.track)}
              trailing={
                <span className="muted small" title={h.scrobbled ? "Scrobbled" : ""}>
                  {new Date(h.playedAt * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}
                  {h.source && h.source !== "youtube" && h.source in SOURCE_LABELS
                    ? ` · ${SOURCE_LABELS[h.source as AudioSource]}`
                    : ""}
                  {h.scrobbled ? " ✓" : ""}
                </span>
              }
            />
          ))}
          {history.length === 0 && <p className="muted">Nothing played yet.</p>}
        </div>
      )}
    </div>
  );
}
