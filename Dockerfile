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
RUN useradd --system --uid 10001 botsu \
    && mkdir /data && chown botsu /data && chmod 0777 /data
COPY --from=build /botsu /usr/local/bin/botsu
# SQLite database (alert subscriptions, posted items). Mount a volume here to keep it.
# World-writable so the image also runs with --user 99:100 (Unraid) or any other user.
ENV BOTSU_DATA_DIR=/data
VOLUME /data
USER botsu
ENTRYPOINT ["/usr/local/bin/botsu"]
