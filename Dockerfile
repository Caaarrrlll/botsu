# syntax=docker/dockerfile:1

# =================================================================================================
# Stage 1: chef - Rust toolchain, build dependencies and cargo-chef
# =================================================================================================
FROM rust:1.100-bookworm AS chef

# Build dependencies:
#   - glib, libva and pkg-config: livekit/libwebrtc, pulled in by fluxer-rust
#   - libopus: songbird (Discord voice)
RUN apt-get update \
    && apt-get install \
        --yes \
        --no-install-recommends \
        libglib2.0-dev \
        libopus-dev \
        libva-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# cargo-chef splits the build so the dependencies get a layer of their own (below).
RUN cargo install \
    --locked \
    --version 0.1.78 \
    cargo-chef

WORKDIR /src


# =================================================================================================
# Stage 2: planner - the dependency recipe
# =================================================================================================
# The dependencies are most of the ~10-15 minute build, including the libwebrtc download. With
# them in their own layer, CI's layer cache keeps them, so a change to botsu's own code only
# rebuilds botsu. The layer is rebuilt when Cargo.toml, Cargo.lock or vendor/ change.
FROM chef AS planner

COPY . .

RUN cargo chef prepare \
    --recipe-path recipe.json


# =================================================================================================
# Stage 3: build - dependencies first (cached), then botsu
# =================================================================================================
FROM chef AS build

COPY --from=planner /src/recipe.json recipe.json

# The recipe only covers workspace members; the vendored fluxer-rust ([patch.crates-io]) must be
# there for cargo to resolve the workspace.
COPY vendor vendor

# Dependencies only: this is the layer the cache keeps.
RUN cargo chef cook \
    --release \
    --package botsu \
    --recipe-path recipe.json

# botsu itself.
COPY . .

RUN cargo build \
        --release \
        --package botsu \
    && cp target/release/botsu /botsu


# =================================================================================================
# Stage 4: runtime image
# =================================================================================================
FROM debian:bookworm-slim

# Runtime libraries:
#   - ca-certificates: HTTPS to Discord, Fluxer, YouTube and Twitch
#   - glib and libva: libwebrtc
#   - libopus: Discord voice
# yt-dlp isn't in the image: botsu downloads it into /data/bin on first start and updates it from
# there, so it never needs a rebuild.
RUN apt-get update \
    && apt-get install \
        --yes \
        --no-install-recommends \
        ca-certificates \
        libglib2.0-0 \
        libopus0 \
        libva-drm2 \
        libva2 \
    && rm -rf /var/lib/apt/lists/*

# ffmpeg decodes audio for voice. A single static binary (~135 MB) instead of Debian's package,
# whose desktop dependencies made the image 830 MB.
COPY --from=mwader/static-ffmpeg:7.1.1 /ffmpeg /usr/local/bin/ffmpeg

# Deno: yt-dlp needs a JavaScript runtime for full YouTube support (its "EJS" challenge solver).
COPY --from=denoland/deno:bin-2.9.8 /deno /usr/local/bin/deno

# The bot and its entrypoint.
COPY --from=build /botsu /usr/local/bin/botsu
COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh

# Settings and data:
#   - /data holds the database and, if settings aren't passed as environment variables, the
#     settings file botsu.env (created on first start). Mount a host folder here to keep them.
#   - The entrypoint gives /data to PUID:PGID and drops to that user before the bot starts.
#   - Caches (yt-dlp, Deno) live in /data too: the bot runs as PUID, which has no writable home.
ENV BOTSU_DATA_DIR=/data \
    XDG_CACHE_HOME=/data/cache \
    DENO_DIR=/data/cache/deno \
    PUID=99 \
    PGID=100

RUN mkdir /data

VOLUME /data

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
