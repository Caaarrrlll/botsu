# syntax=docker/dockerfile:1
FROM rust:1.100-bookworm AS build
# Build deps: livekit/libwebrtc (pulled in by fluxer-rust) and libopus (songbird, Discord voice).
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libglib2.0-dev libva-dev libopus-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p botsu && cp target/release/botsu /botsu

FROM debian:bookworm-slim
# Runtime: libwebrtc libs and libopus. yt-dlp isn't in the image: botsu downloads it into
# /data/bin on first start and updates it from there, so it never needs a rebuild.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libglib2.0-0 libva2 libva-drm2 libopus0 \
    && rm -rf /var/lib/apt/lists/*
# ffmpeg decodes audio for voice. A single static binary (~135 MB) instead of Debian's package,
# whose desktop dependencies made the image 830 MB.
COPY --from=mwader/static-ffmpeg:7.1.1 /ffmpeg /usr/local/bin/ffmpeg
# Deno: yt-dlp needs a JavaScript runtime for full YouTube support (its "EJS" challenge solver).
COPY --from=denoland/deno:bin-2.9.8 /deno /usr/local/bin/deno
COPY --from=build /botsu /usr/local/bin/botsu
COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh
# /data holds the database and, if settings aren't passed as environment variables, the
# settings file botsu.env (created on first start). Mount a host folder here to keep them.
# The entrypoint gives /data to PUID:PGID and drops to that user before the bot starts.
# Caches (yt-dlp, Deno) live in /data too: the bot runs as PUID, which has no writable home.
ENV BOTSU_DATA_DIR=/data \
    XDG_CACHE_HOME=/data/cache \
    DENO_DIR=/data/cache/deno \
    PUID=99 \
    PGID=100
RUN mkdir /data
VOLUME /data
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
