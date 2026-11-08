#!/usr/bin/env bash
# Runs the ytp server and the Vite dev server (which forwards /api to it) together.
# Open the Vite address it prints (port 5173); stop both with Ctrl-C.
#
# On Windows this runs inside WSL: from Git Bash it hands off to scripts/dev.cmd, which mounts
# the drive in WSL, installs the toolchain on first run (scripts/setup-wsl.sh) and comes back here.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) exec cmd.exe //c "$(cygpath -w "$root/scripts/dev.cmd")" ;;
esac

# rustup / Go / Node installs often aren't on PATH in a fresh shell.
for d in "$HOME/.cargo/bin" "$HOME/.local/go/bin" "/usr/local/go/bin" "$HOME/go/bin" "$HOME/.local/node/bin"; do
  [[ -d "$d" && ":$PATH:" != *":$d:"* ]] && PATH="$d:$PATH"
done
export PATH

wsl=""
grep -qi microsoft /proc/version 2>/dev/null && wsl=1
hint="install it"
[[ -n "$wsl" ]] && hint="run scripts/setup-wsl.sh"

if ! command -v cargo >/dev/null; then
  echo "error: cargo not found — $hint, or install Rust from https://rustup.rs (or add ~/.cargo/bin to PATH)" >&2
  exit 1
fi
# WSL puts Windows' npm on PATH; it can't run the Linux Vite install, so insist on a Linux one.
if ! npm_path="$(command -v npm)" || [[ "$npm_path" == /mnt/* ]]; then
  echo "error: Node.js (Linux build) not found — $hint, or install Node 20+" >&2
  exit 1
fi

# Is the project on a network share? A Windows drive under WSL, or a GVFS mount on Linux
# (e.g. /run/user/1000/gvfs/smb-share:server=…). GVFS paths contain ':', which cargo can't put
# into LD_LIBRARY_PATH for `cargo run`, so the build output must live elsewhere there anyway.
on_share=""
[[ -n "$wsl" && "$root" == /mnt/* ]] && on_share=1
[[ "$root" == */gvfs/* || "$root" == *:* ]] && on_share=1

if [[ -n "$on_share" ]]; then
  # Keep Rust's build output on the local filesystem (far faster than building over
  # drvfs/SMB, and free of ':' in its path), and poll for changes because edits made on
  # another machine (or from Windows) don't produce file-change events here.
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/ytp/server-target}"
  export CHOKIDAR_USEPOLLING="${CHOKIDAR_USEPOLLING:-1}"
  # yt-dlp is a 40 MB self-unpacking binary read in full on every run (each new song): ~6 s from
  # the share vs <1 s locally. Keep the managed copy on the Linux side, seeded from the share's.
  export YTP_BIN_DIR="${YTP_BIN_DIR:-$HOME/.cache/ytp/bin}"
  if [[ ! -e "$YTP_BIN_DIR/yt-dlp" && -f "$root/server/data/bin/yt-dlp" ]]; then
    mkdir -p "$YTP_BIN_DIR"
    cp "$root/server/data/bin/yt-dlp" "$YTP_BIN_DIR/yt-dlp" && chmod +x "$YTP_BIN_DIR/yt-dlp"
  fi
  # Network shares (drvfs, GVFS) can't hold symlinks, so npm can't link node_modules/.bin.
  npm_ci_flags="--no-bin-links"
fi

if [[ ! -d "$root/web/node_modules" ]]; then
  echo "==> installing web dependencies…"
  (cd "$root/web" && npm ci ${npm_ci_flags:-})
fi

# `npm run dev` needs node_modules/.bin/vite, a symlink that's missing (or invisible) when the
# project is on a share that can't hold symlinks — run Vite's entry point directly then.
vite_cmd=(npm run dev)
[[ -e "$root/web/node_modules/.bin/vite" ]] || vite_cmd=(node node_modules/vite/bin/vite.js)

# The server's address, from server/.env (YTP_BIND), so Vite forwards /api to the right place.
bind="$(grep -E '^YTP_BIND=' "$root/server/.env" 2>/dev/null | tail -1 | cut -d= -f2- || true)"
bind="${bind:-127.0.0.1:8080}"
bind="${bind/0.0.0.0/127.0.0.1}"
export YTP_BACKEND="${YTP_BACKEND:-http://$bind}"

echo "==> building and starting the ytp server (first build takes a few minutes)…"
(cd "$root/server" && cargo run) &
backend=$!
trap 'kill $backend 2>/dev/null || true' EXIT

# Wait for the server before starting Vite, and stop if it fails to build or start.
until curl -fs "$YTP_BACKEND/api/auth/check" >/dev/null 2>&1; do
  if ! kill -0 "$backend" 2>/dev/null; then
    wait "$backend" || true
    echo "error: the ytp server exited before it started — see the output above" >&2
    exit 1
  fi
  sleep 1
done
echo "==> ytp server is up at $YTP_BACKEND; starting Vite"

(cd "$root/web" && exec "${vite_cmd[@]}") &
vite=$!
trap 'kill $backend $vite 2>/dev/null || true' EXIT

# If either one stops, stop the other too (no Vite left running without its server).
wait -n "$backend" "$vite" || true
if ! kill -0 "$backend" 2>/dev/null; then
  echo "error: the ytp server stopped — stopping Vite too" >&2
  exit 1
fi
