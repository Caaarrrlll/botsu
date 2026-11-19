# botsu

A lean, self-hosted chat bot in Rust, replacing a Red-DiscordBot instance. It runs on
**Discord** and **[Fluxer](https://fluxer.app)** from one platform-neutral core (Stoat is a
possible bonus). Plan: [docs/roadmap.md](docs/roadmap.md). How it works inside (crates, database
tables, alert rules, extending): [docs/architecture.md](docs/architecture.md). Background:
[docs/handover.md](docs/handover.md).

## Status

| Phase | State |
|---|---|
| 0. Spikes (voice on Fluxer, Discord DAVE) | open, see [spikes/](spikes/README.md) |
| 1. Skeleton on Discord + Fluxer | `!ping` works on both; Docker image still to verify |
| 2. Alerts (YouTube uploads, Twitch live) | code done; live test pending |
| 3. CI/CD (GitHub Actions → Docker Hub) and install guides | done once the Docker Hub token secret is set |
| 4. Server tools (permissions, cleanup, admin) | |
| 5. Moderation (cases, temp-bans, warnings) | |
| 6. Music (yt-dlp + ffmpeg → Discord voice / LiveKit) | |
| 7. Cutover from Red | |

## Run locally

```sh
cp .env.example .env   # fill in DISCORD_TOKEN and/or FLUXER_TOKEN
set -a; . ./.env; set +a
cargo run -p botsu
```

Then type `!ping` in a channel the bot can see (`.env.example` sets `BOTSU_PREFIX=!` so botsu doesn't clash with Red; the built-in default is `$`).

The first build is slow: `fluxer-rust` pulls in LiveKit, which downloads and links libwebrtc.

## Configuration

All configuration is environment variables; see [.env.example](.env.example).

| Variable | Required | Default |
|---|---|---|
| `DISCORD_TOKEN` | one of the two | enables Discord |
| `FLUXER_TOKEN` | one of the two | enables Fluxer |
| `FLUXER_URL` | no | your self-hosted instance's web address; API URL is discovered. Unset = fluxer.app |
| `FLUXER_API_URL` | no | explicit API base with `/v1`; overrides `FLUXER_URL` |
| `BOTSU_PREFIX` | no | `$` |
| `BOTSU_OWNERS` | for import | user IDs (any platform) allowed to import alert lists and toggle Shorts |
| `YOUTUBE_API_KEY` | recommended | YouTube Data API v3 key: @handles, live/upcoming stream detection |
| `TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET` | for Twitch alerts | from dev.twitch.tv; both or neither |
| `BOTSU_DATA_DIR` | no | `data` (`/data` in Docker): SQLite database |
| `YOUTUBE_POLL_SECONDS`, `TWITCH_POLL_SECONDS` | no | `300`, `120` |
| `RUST_LOG` | no | `info,botsu=debug` |

Getting tokens and testing against live servers: [docs/testing.md](docs/testing.md).

## Docker / Unraid

Image: `caaarrrlll/botsu:latest` on Docker Hub, built and published by GitHub Actions on every push
to `master` once the checks pass (`sha-<commit>` tags too, for rollbacks).
Guides: [Unraid](docs/install-unraid.md) (template) · [any Docker host](docs/install-docker.md)
(`docker run` / Compose) · [native Linux](docs/install-linux.md) (systemd, no containers).

Any Docker host:

```sh
docker run -d --name botsu --restart unless-stopped \
  -v /path/to/botsu-data:/data \
  caaarrrlll/botsu:latest
```

On first start the container creates `botsu.env` in the data folder and waits: fill in at least
one token (no quotes around values) and it starts within 30 seconds. Settings can instead be
passed with `-e` or `--env-file`; those override the file. `/data` also holds the SQLite
database (subscriptions, what's been posted), so keep it on a volume.

The container runs the bot as `PUID:PGID` (default `99:100`, Unraid's nobody:users) and fixes the
data folder's ownership on start. Set `-e PUID=1000 -e PGID=1000` (or your own IDs) elsewhere.
Build it yourself with `docker build -t botsu .`.

## Layout

```
crates/botsu-core    platform-neutral core (no platform dependencies)
crates/botsu-discord Discord adapter (only crate using serenity)
crates/botsu-fluxer  Fluxer adapter (only crate using fluxer-rust)
crates/botsu         binary: config, logging, adapter tasks
spikes/              throwaway phase-0 experiments
```

Details (database tables, alert rules, adding a platform or command): [docs/architecture.md](docs/architecture.md).
