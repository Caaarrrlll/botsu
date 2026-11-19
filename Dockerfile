# syntax=docker/dockerfile:1
FROM rust:1.98-bookworm AS build
# Build deps for livekit/libwebrtc (pulled in by fluxer-rust).
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libglib2.0-dev libva-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p botsu && cp target/release/botsu /botsu

FROM debian:bookworm-slim
# Runtime libs for libwebrtc. Phase 4 adds ffmpeg here, and yt-dlp installed
# into a volume/venv so it can be updated without rebuilding the image.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libglib2.0-0 libva2 libva-drm2 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /botsu /usr/local/bin/botsu
COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh
# /data holds the database and, if settings aren't passed as environment variables, the
# settings file botsu.env (created on first start). Mount a host folder here to keep them.
# The entrypoint gives /data to PUID:PGID and drops to that user before the bot starts.
ENV BOTSU_DATA_DIR=/data \
    PUID=99 \
    PGID=100
RUN mkdir /data
VOLUME /data
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
