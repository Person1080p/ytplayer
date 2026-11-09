import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const backend = process.env.YTP_BACKEND ?? "http://127.0.0.1:8080";

// Cache-busting version for the in-page Tailscale module (public/tailnet/tailnet.wasm).
const wasmPath = new URL("./public/tailnet/tailnet.wasm", import.meta.url);
const tailnetVersion = existsSync(wasmPath)
  ? createHash("sha256").update(readFileSync(wasmPath)).digest("hex").slice(0, 12)
  : "missing";

// The git commit this page is built from, for the version check in Settings. Git may refuse a repo
// on a network share owned by someone else ("dubious ownership"), hence `safe.directory`; with no
// git at all the commit is just unknown.
const here = fileURLToPath(new URL(".", import.meta.url));
function git(...args: string[]): string | null {
  for (const pre of [["-c", "safe.directory=*"], []]) {
    try {
      return execFileSync("git", [...pre, ...args], { cwd: here, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 20_000 }).trim();
    } catch {
      // try the next way, or give up
    }
  }
  return null;
}
const commit = process.env.YTP_COMMIT || git("rev-parse", "HEAD") || null;
const dirty = !process.env.YTP_COMMIT && !!commit && !!git("status", "--porcelain");

export default defineConfig({
  define: {
    __TAILNET_VERSION__: JSON.stringify(tailnetVersion),
    __APP_COMMIT__: JSON.stringify(commit),
    __APP_DIRTY__: JSON.stringify(dirty),
    __APP_BUILT_AT__: JSON.stringify(Math.floor(Date.now() / 1000)),
  },
  plugins: [
    react(),
    VitePWA({
      registerType: "autoUpdate",
      includeAssets: ["icon.svg", "apple-touch-icon.png"],
      manifest: {
        name: "YTP Music",
        short_name: "YTP",
        description: "Self-hosted music player: YouTube Music, Plex and Last.fm",
        theme_color: "#0f0f12",
        background_color: "#0f0f12",
        display: "standalone",
        orientation: "portrait",
        start_url: "/",
        scope: "/",
        icons: [
          { src: "icon-192.png", sizes: "192x192", type: "image/png" },
          { src: "icon-512.png", sizes: "512x512", type: "image/png" },
          { src: "icon-maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
        ],
      },
      workbox: {
        // API calls (and especially audio streams) must always hit the server.
        navigateFallbackDenylist: [/^\/api\//],
        // The Tailscale module (~28 MB) is precached with the app: a new version doesn't take over
        // until it's stored, so the fallback can always start away from home, where this address
        // can't serve it. Unchanged between versions, it isn't downloaded again.
        globIgnores: ["**/*.gz"],
        maximumFileSizeToCacheInBytes: 64 * 1024 * 1024,
        // The page asks for `/tailnet/…?v=<hash>`; the precache keeps it under the plain path.
        ignoreURLParametersMatching: [/^utm_/, /^fbclid$/, /^v$/],
        runtimeCaching: [
          {
            // Fallback for the precached Tailscale module (versioned URL, so cached forever).
            urlPattern: ({ url }) => url.pathname.startsWith("/tailnet/"),
            handler: "CacheFirst",
            options: { cacheName: "tailnet", expiration: { maxEntries: 4 }, cacheableResponse: { statuses: [200] } },
          },
          {
            urlPattern: ({ url }) => /googleusercontent\.com|ggpht\.com|ytimg\.com/.test(url.hostname),
            handler: "CacheFirst",
            options: {
              cacheName: "artwork",
              expiration: { maxEntries: 800, maxAgeSeconds: 60 * 60 * 24 * 30 },
              cacheableResponse: { statuses: [0, 200] },
            },
          },
        ],
      },
    }),
  ],
  server: {
    host: true,
    proxy: { "/api": { target: backend, changeOrigin: false } },
  },
});
