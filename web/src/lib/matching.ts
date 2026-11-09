// Fuzzy matching of "artist - title" pairs, the same rules as the server's `matching.rs`, for the
// music folder on this device (matched here, since the server can't see it).

const BRACKETS =
  /[([][^)\]]*(official|video|audio|lyric|lyrics|visuali[sz]er|remaster(ed)?|explicit|clean|hd|hq|4k|mv|m\/v|feat\.?|ft\.?|with |prod\.?|from |version|edit)[^)\]]*[)\]]/giu;
const FEAT = /\s+(feat\.?|ft\.?|featuring)\s+.*$/iu;
const DASH_SUFFIX = /\s+-\s+(\d{4}\s+)?remaster(ed)?.*$|\s+-\s+(radio|single|album) (edit|version).*$/iu;
const NON_ALNUM = /[^\p{L}\p{N}]+/gu;

/** Lowercase, strip decorations like "(Official Video)", "feat. X", punctuation. */
export function normalize(s: string): string {
  let t = s.replace(BRACKETS, " ").replace(FEAT, "").replace(DASH_SUFFIX, "");
  t = t.toLowerCase().replaceAll("&", " and ").replaceAll(" - topic", "");
  return t.replace(NON_ALNUM, " ").split(/\s+/).filter(Boolean).join(" ");
}

/** First listed artist ("A, B & C" → "a"). */
export function primaryArtist(s: string): string {
  const first = s.split(/[,;/]/)[0].split(" & ")[0].split(" x ")[0];
  return normalize(first);
}

/** Similarity in 0..1 between two already-normalized strings. */
export function similarity(a: string, b: string): number {
  if (!a || !b) return 0;
  if (a === b) return 1;
  if (a.includes(b) || b.includes(a)) {
    const [short, long] = a.length < b.length ? [a, b] : [b, a];
    return 0.7 + 0.25 * (short.length / long.length);
  }
  const ta = new Set(a.split(" "));
  const tb = new Set(b.split(" "));
  let inter = 0;
  for (const w of ta) if (tb.has(w)) inter++;
  const union = new Set([...ta, ...tb]).size;
  return union ? inter / union : 0;
}

/** How well a candidate matches a wanted track, before clamping (exact title and artist reach 1,
 *  and the length bonus on top tells the right recording from another one). */
export function scoreUnclamped(
  wantTitle: string,
  wantArtist: string,
  wantDuration: number | null | undefined,
  candTitle: string,
  candArtist: string,
  candDuration: number | null | undefined,
): number {
  const wt = normalize(wantTitle);
  const ct = normalize(candTitle);
  const title = similarity(wt, ct);
  const wa = primaryArtist(wantArtist);
  const caFull = normalize(candArtist);
  const artist =
    wa && caFull.includes(wa)
      ? 1
      : Math.max(similarity(wa, primaryArtist(candArtist)), wa && ct.includes(wa) ? 0.8 : 0);
  let s = title * 0.6 + artist * 0.4;
  if (wantDuration != null && candDuration != null) {
    const diff = Math.abs(wantDuration - candDuration);
    if (diff <= 3) s += 0.1;
    else if (diff > 20) s -= 0.25;
    s -= Math.min(diff, 30) * 0.002;
  }
  return s;
}

const LIVE_WORDS =
  /\b(live|concert|tour|festival|session|sessions|unplugged|bootleg|in concert|acoustic)\b|\b\d{4}[.\-/]\d{2}[.\-/]\d{2}\b|\b\d{2}[.\-/]\d{2}[.\-/]\d{4}\b/i;

/** A live, session or acoustic recording, judging by its title or album. */
export function looksLive(title: string, album?: string | null): boolean {
  return LIVE_WORDS.test(title) || (!!album && LIVE_WORDS.test(album));
}

/** Like the server's `plex::rank`: (score, checked against the match threshold; rank, which also
 *  prefers the same album and the studio recording, for choosing among copies). */
export function rank(
  want: { title: string; artist: string; album?: string | null; duration?: number | null },
  cand: { title: string; artist: string; albumArtist?: string | null; album?: string | null; duration?: number | null },
): [number, number] {
  const s = (artist: string) => scoreUnclamped(want.title, want.artist, want.duration, cand.title, artist, cand.duration);
  const base = Math.max(s(cand.artist), cand.albumArtist ? s(cand.albumArtist) : -Infinity);
  let r = base;
  if (want.album && cand.album) r += 0.15 * similarity(normalize(want.album), normalize(cand.album));
  if (looksLive(cand.title, cand.album) && !looksLive(want.title, want.album)) r -= 0.2;
  return [Math.min(1, Math.max(0, base)), r];
}
