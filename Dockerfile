# syntax=docker/dockerfile:1
# YTP Music in one image: the Rust server (with its built-in Tailscale node), the web app (with its
# in-page Tailscale module), ffmpeg for the audio cache and Deno, which yt-dlp needs for YouTube.
#   docker compose up -d --build     (see docker-compose.yml)

# The commit being built, for the version shown in Settings (.git isn't in the build context).
ARG GIT_COMMIT=

FROM golang:1.27.3-bookworm AS tailnet
WORKDIR /src/web/tailnet
COPY web/tailnet/go.mod web/tailnet/go.sum ./
RUN go mod download
COPY web/tailnet/ ./
RUN GOOS=js GOARCH=wasm go build -tags "$(go run ./internal/tags)" -trimpath -ldflags "-s -w" -o /out/tailnet.wasm . \
 && js="$(go env GOROOT)/lib/wasm/wasm_exec.js" && { [ -f "$js" ] || js="$(go env GOROOT)/misc/wasm/wasm_exec.js"; } \
 && cp "$js" /out/

FROM node:22-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
# Declared after npm ci: a new commit shouldn't reinstall the packages.
ARG GIT_COMMIT
COPY web/ ./
COPY --from=tailnet /out/ public/tailnet/
RUN YTP_COMMIT="$GIT_COMMIT" npm run build

# The commit isn't passed to this stage (the server reads it from the image, see the last stage),
# so it stays cached when server/ is unchanged; target/ is kept between builds, so a server change
# recompiles only the server itself, not every dependency.
FROM rust:1.99.1-bookworm AS server
# Go builds the embedded Tailscale node (server/tsnet-ffi) as a C archive linked into the server.
COPY --from=golang:1.27.3-bookworm /usr/local/go /usr/local/go
ENV PATH=/usr/local/go/bin:$PATH
WORKDIR /src/server
COPY server/ ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/root/go/pkg/mod \
    --mount=type=cache,target=/src/server/target \
    cargo build --release --locked \
 && cp target/release/ytp-server /out-ytp-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg tzdata \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 1000 --create-home --home-dir /home/ytp ytp \
 && mkdir -p /data && chown ytp:ytp /data
COPY --from=denoland/deno:bin-2.9.8 /deno /usr/local/bin/deno
COPY --from=server /out-ytp-server /app/ytp-server
COPY --from=web /src/web/dist /app/web
# The version shown in Settings (read at runtime: see the server stage).
ARG GIT_COMMIT
ENV YTP_GIT_COMMIT=$GIT_COMMIT
RUN date +%s > /app/built-at
ENV YTP_BIND=0.0.0.0:8080 \
    YTP_DATA_DIR=/data \
    YTP_WEB_DIR=/app/web \
    RUST_LOG=ytp_server=info,tower_http=warn
USER ytp
WORKDIR /home/ytp
EXPOSE 8080
VOLUME /data
ENTRYPOINT ["/app/ytp-server"]
