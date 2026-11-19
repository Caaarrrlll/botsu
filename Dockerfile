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
# Runtime: libwebrtc libs and libopus. Phase 6 adds yt-dlp, installed into a volume/venv so it can
# be updated without rebuilding the image.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libglib2.0-0 libva2 libva-drm2 libopus0 \
    && rm -rf /var/lib/apt/lists/*
# ffmpeg decodes audio for voice. A single static binary (~135 MB) instead of Debian's package,
# whose desktop dependencies made the image 830 MB.
COPY --from=mwader/static-ffmpeg:7.1.1 /ffmpeg /usr/local/bin/ffmpeg
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
