# YTP Music

A self-hosted music player that you install as a PWA on your phone. The backend is in Rust (axum + SQLite) and the frontend in React (Vite + vite-plugin-pwa).

- **YouTube Music**: search, home feed (mixes, Quick picks, Supermix), playlists and albums, plus automatic "radio" queues.
- **yt-dlp**: managed by the server. It resolves audio and proxies it with HTTP Range support, so seeking works. If direct URLs are blocked, it falls back to piping yt-dlp's output.
- **YouTube history**: tracks you listen to are reported to your YouTube watch history, which feeds YouTube Music's own recommendations.
- **Audio cache**: once a track played from YouTube at high quality has fully loaded, the server converts the audio it already passed through to an MP3 tagged with the song's title, artist, album and cover art (needs `ffmpeg`), and keeps it in `server/data/audio-cache/` as `Artist - Title [videoId].mp3`. Plays during that time come from the copy, shown as **Cached** in the player. How long copies are kept (3 hours by default, forever, or off) is set in **Settings → Playback**.
- **Plex / Plexamp**: if a track also exists in your Plex library, it streams from Plex instead. Codecs the browser can't play are transcoded to MP3.
- **Last.fm**: now-playing updates, scrobbling with offline retry, loves, and similar-track recommendations blended into radio and autoplay.
- **Queue**: kept on the server and shared live across devices over SSE. It supports play now / play next / add to queue, drag-to-reorder, shuffle, autoplay top-up, and saving as a playlist.
- **Imports**: liked tracks from YouTube Music, Deezer, **Demus** export files, or any CSV/JSON/M3U/plist/zip export (Exportify, TuneMyMusic, Soundiiz, …). Tracks are matched to YouTube automatically, and YouTube video IDs inside an export are used directly.
- **Tailscale in the web page**: the PWA runs its own Tailscale node, compiled from Go to WebAssembly, so a phone can reach the server **without the Tailscale app or any OS VPN**. The server joins your tailnet by itself too, with a built-in Tailscale node that is on by default. A host `tailscaled` also works.

```
phone (Tailscale app) ──https──▶ tailscale serve / embedded tsnet ──▶ ytp-server ─┬─ YouTube Music InnerTube API
                                                                                    ├─ yt-dlp ─▶ googlevideo (audio proxy)
                                                                                    ├─ Plex server (audio proxy)
                                                                                    ├─ Last.fm API
                                                                                    └─ SQLite (queue, history, library, settings)
```

## Quick start

Requirements: Rust (stable), Node 20+, and Go. Go builds the server's built-in Tailscale node and the web app's in-page Tailscale fallback.

```bash
cd web && npm install && npm run build && cd ..
cd server && cp .env.example .env && cargo run --release
# open http://127.0.0.1:8080
```

On first start the server downloads the standalone yt-dlp binary into `server/data/bin/`, unless `YTDLP_PATH` is set or `yt-dlp` is already on your `PATH`. You can update it from **Settings → yt-dlp**.

For development, `scripts/dev.sh` runs the backend and Vite at http://localhost:5173, with `/api` proxied.

For the in-page Tailscale mode, also install Go and run `scripts/build-tailnet-wasm.sh` before `npm run build`.

### Docker (e.g. on a NAS)

```bash
FIREFOX_PASSWORD=<password> YTP_ACCESS_TOKEN=<a long random string> TZ=America/New_York docker compose up -d --build
# app: http://<host>:8080 · the server's browser: https://<host>:5800 (user "ytp")
```

The image builds everything itself (server, embedded Tailscale node, web app with its in-page Tailscale module) and includes ffmpeg and Deno, which yt-dlp needs for YouTube.

- **Data.** Everything lives in the `data` volume at `/data`. To move an install, copy `server/data` there, owned by uid 1000. `bin/` (yt-dlp) and `logs/` can be left out.
- **Access token.** In Docker the server listens beyond localhost, so set `YTP_ACCESS_TOKEN`.
- **Sign in with Google.** A container has no desktop, so the stack includes a **Firefox you use through a web page** (`jlesage/firefox`, port 5800).
  - It's protected by HTTPS (self-signed) and a login page with `FIREFOX_PASSWORD`, because it's signed in to your Google account.
  - Its profile is mounted read-only into the server, where yt-dlp finds it.
  - In Settings, **Open the server's browser** opens it. Sign in to music.youtube.com there, then quit Firefox from its menu: it reopens by itself, and quitting saves the new sign-in to disk. Back in Settings, the import then finishes.
  - Leave it running so "keep in sync" keeps getting fresh cookies.
  - `YTP_SIGNIN_BROWSER_URL` sets the address the button opens.
- **Spotify Downloads-folder pickup** doesn't work in a container (there's no Downloads folder).
- **Installing it as an app.** Browsers only allow this over HTTPS. Use the embedded node's `https://ytp.<tailnet>.ts.net` address.
- **Version.** Pass the commit as the `YTP_GIT_COMMIT` variable to show it in Settings.

## Getting it on your phone

There are two ways a device can reach the server:

- **Direct (always preferred)**: the app talks to the server at the page's own address. If that address answers, it's used, every time.
- **Tailscale fallback**: used **only** when the page's address can't reach the server. The page then runs its own Tailscale node and connects over your tailnet.

The app re-checks on launch, on resume, on network changes, and when direct requests fail, and every 60 seconds while on the fallback. It switches between the two automatically, so a PWA installed from your server at home keeps working away from home: it opens from the service-worker cache, finds the address unreachable, and falls back to Tailscale. The Tailscale module is never even downloaded while direct works. Set up or check the fallback in **Settings → Connection**.

### No VPN on the phone: in-page Tailscale fallback

The web app includes a full Tailscale client built from Go to WebAssembly (`web/tailnet/`, adapted from Tailscale's own in-browser client, `tsconnect`). The page joins your tailnet like any other device, so the phone needs **no Tailscale app and no VPN profile**.

```
phone browser / PWA ──https──▶ public static host (just the app files, no data)
        │
        └─ in-page Tailscale (WASM) ══ WireGuard over WebSocket via Tailscale DERP ══▶ ytp-server on your tailnet
```

1. Build the WASM module (needs Go): `scripts/build-tailnet-wasm.sh`, then `cd web && npm run build`.
2. Host `web/dist/` somewhere reachable from the phone over HTTPS. It's only static files with no secrets and no API. Options:
   - **Tailscale Funnel** on the server, serving only the static files publicly, while the API stays tailnet-only:
     `tailscale funnel --bg --set-path / /path/to/web/dist`
   - Any static host (GitHub Pages, Cloudflare Pages, Netlify, …). Configure SPA fallback to `index.html` if the host supports it.
3. Open that URL on the phone. The app opens normally, with a banner saying the server isn't reachable from this address. Tap **Set up connection**, or go to **Settings → Connection → Set up Tailscale fallback**, then:
   **Connect to Tailscale** → **Sign in** with your Tailscale account (or paste an auth key) → pick the server from the device list → **Save**. The server field defaults to the server this app talks to. While connected directly, the app asks the server for its own tailnet address (its `tailscale serve` HTTPS name, its MagicDNS name and port if it listens beyond localhost, or the embedded node's name) and remembers it. If there isn't one, the page's own address is used. Then use "Add to Home Screen". Setup never blocks the rest of the app: you can leave Settings mid-way, and the node keeps connecting in the background.
4. Once a fallback is saved, the app quietly downloads the Tailscale module while it's still reachable directly, so the service worker has it cached for when you're away. Nothing runs until it's needed.
5. The node is persisted in the browser's storage, so later launches reconnect silently.

Which server address to enter:

| Server setup | Address |
|---|---|
| Embedded tailscale (`TS_EMBEDDED=1`) | `ytp` (plain HTTP on tailnet port 80; WireGuard encrypts it) |
| Host tailscaled, server bound to the tailnet IP or `0.0.0.0` | `100.x.y.z:8080` or `<machine>:8080` |
| Host tailscaled + `tailscale serve --bg 8080` | `https://<machine>.<tailnet>.ts.net` |

How it works:
- Browsers can't send UDP, so the in-page node's traffic is always relayed through Tailscale's DERP servers over WebSockets. It's still end-to-end WireGuard encrypted. Bandwidth is fine for music, but latency is higher than a direct connection.
- API calls and the live queue stream go through the tunnel with `fetch()`. Audio can't be handed to `<audio>` by URL in this mode, so each track is downloaded into memory over the tunnel and the next track is prefetched. That also keeps auto-advance working while iOS has the app backgrounded.
- The WASM module is about 28 MB (6.7 MB gzipped). It's downloaded once and cached by the service worker.
- If your tailnet uses device approval or Tailnet Lock, approve or sign the new device in the admin console.

### With the Tailscale app on the phone (direct mode)

```bash
tailscale up
tailscale serve --bg 8080     # https://<machine>.<tailnet>.ts.net → 127.0.0.1:8080
```

Open that URL on the phone and the app talks to the server directly. HTTPS matters: browsers only allow installing the PWA and running the service worker on a secure origin. You need to enable HTTPS certificates once in the Tailscale admin console.

### Putting the server on your tailnet

The Tailscale fallback can only reach the server if the **server itself is on your tailnet**. **Settings → Connection** shows whether it is ("Server on your tailnet: yes/no"). There are two ways.

**A. Built-in (the default, no Tailscale install on the server machine)**

```bash
cd server && cargo build --release      # compiles the embedded node from server/tsnet-ffi (needs Go)
./target/release/ytp-server
```

The server contains its own Tailscale node, built from a small Go wrapper around `tailscale.com/tsnet` in [`server/tsnet-ffi`](server/tsnet-ffi), and it's on by default.

- The server joins your tailnet as `ytp` (`TS_HOSTNAME`). To sign it in, open **Settings → Connection** in the web app and click **Sign the server in to Tailscale**. The link is also printed in the server log. Alternatively, set `TS_AUTHKEY=tskey-auth-…` to join without signing in.
- It serves plain HTTP on tailnet port 80, so the fallback server address is simply `http://ytp`. WireGuard already encrypts this traffic.
- If HTTPS certificates are enabled for your tailnet in the admin console, it also serves `https://ytp.<tailnet>.ts.net`, using a certificate it renews daily.
- To turn it off at runtime, set `TS_EMBEDDED=0`. To build without it (and without needing Go), run `cargo build --no-default-features`.

**B. Tailscale installed on the server machine**

```bash
tailscale up
tailscale serve --bg 8080     # https://<machine>.<tailnet>.ts.net → 127.0.0.1:8080
```

## Connecting accounts

All of these are set up in the app under **Settings**, and secrets are stored only in the server's SQLite database.

| Service | How |
|---|---|
| YouTube Music | Click **Sign in with Google** on the computer running the server. Sign in on Google's own page, then come back and the server picks up the sign-in from that computer's browser (Firefox, Chrome, Chromium, Brave, Edge, Vivaldi or Opera, read via yt-dlp). With "keep in sync" on, it re-imports every 6 hours, so it doesn't expire while the browser stays signed in. From another device, paste the `cookie` request header instead. Either way, a sign-in is only saved after YouTube confirms which account it belongs to. Search and radio work without it. |
| Plex | "Sign in with Plex" (PIN flow) → pick a server connection. You can also enter a URL and token manually. |
| Last.fm | Create an API account, enter the key and secret (or set `LASTFM_API_KEY` / `LASTFM_API_SECRET`), then click Connect. You'll be returned to `<app url>/oauth/lastfm`. |
| Spotify playlists | Nothing to set up. Paste a playlist link on the Playlists page: the first 100 songs come from Spotify's public page and stay in sync. For longer playlists, “Sync now” copies the link and opens Chosic's playlist exporter; download the CSV there and the server picks it up from your Downloads folder (Settings → Spotify playlists shows or changes the folder). |
| Deezer | Library → Import → enter your profile ID or URL. The profile must be public, or you can supply an access token. |
| Demus | In Demus, export or share your library or a playlist and save the file. Then go to Library → Import → Demus → Choose export…. Demus doesn't document its export format, so the importer unpacks zip, gzip, binary/XML plist (including `NSKeyedArchiver`), SQLite/Core Data, JSON and text, and extracts anything track-shaped. YouTube video IDs are used directly. If an export doesn't import, please open an issue with a sample file. |
| Anything else | Export to CSV or "Artist - Title" lines and use Library → Import → File. |

## Configuration

See [`server/.env.example`](server/.env.example). If the server is reachable beyond your tailnet, set `YTP_ACCESS_TOKEN` so the app requires a token.

Cargo features:

- `embedded-tailscale` (**default**): the server's built-in Tailscale node (see above). Needs Go at build time; set `YTP_GO` if `go` isn't on your `PATH`.
- `embed-ytdlp`: bakes `server/vendor/yt-dlp` (from `scripts/fetch-ytdlp.sh`) into the executable, giving you a single-file deployment.

## How it fits together

- `server/src/services/ytmusic.rs`: InnerTube (`WEB_REMIX`) client. It parses responses by walking the JSON for renderer types instead of fixed paths, so it survives most layout changes.
- `server/src/services/ytdlp.rs`: finds or installs yt-dlp, resolves stream URLs (cached for 4 hours), and handles pipe mode.
- `server/src/routes/stream.rs`: `/api/stream/:videoId` serves the Plex copy if one matches, otherwise the proxied googlevideo URL, otherwise yt-dlp's stdout.
- `server/src/services/recommend.rs`: blends YouTube radio and Last.fm similar tracks 2:1, removing duplicates and recent plays.
- `server/src/services/playback.rs`: applies Last.fm's scrobble rule (more than half the track or 4 minutes, track over 30s), counts only real listening time (seeking doesn't count), retries failed scrobbles, and pings YouTube history.
- `server/src/services/matching.rs`: fuzzy artist/title matching used for Plex lookups and imports.
- `web/src/lib/player.ts`: the audio element, Media Session (lock screen, headphones, car), progress reporting, and following the server queue.
- `web/tailnet/main.go`: the in-page Tailscale node (WASM), with a binary streaming `fetch()`. `web/src/lib/tailnet.ts` loads it and wraps it as a standard `Response`. `web/src/lib/transport.ts` decides between direct and tailnet for every API call.
- `server/src/services/fileimport.rs`: the format-sniffing importer used for Demus and other export files.

## Caveats

- YouTube Music has no public API. The InnerTube endpoints and the cookie login can break when Google changes them, and using them may conflict with YouTube's Terms of Service. This project is intended for personal use.
- iOS suspends PWAs aggressively. Background playback works, but auto-advancing to the next track while the screen is locked is less reliable than on Android.
