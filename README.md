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
| 3. CI/CD (GitHub Actions → Docker Hub) and install guides | |
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
| `BOTSU_OWNERS` | for alerts | user IDs (any platform) allowed to add/remove alerts |
| `YOUTUBE_API_KEY` | recommended | YouTube Data API v3 key: @handles, live/upcoming stream detection |
| `TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET` | for Twitch alerts | from dev.twitch.tv; both or neither |
| `BOTSU_DATA_DIR` | no | `data` (`/data` in Docker): SQLite database |
| `YOUTUBE_POLL_SECONDS`, `TWITCH_POLL_SECONDS` | no | `300`, `120` |

Getting tokens and testing against live servers: [docs/testing.md](docs/testing.md).
| `RUST_LOG` | no | `info,botsu=debug` |

## Docker / Unraid

Step-by-step for Unraid (secrets file, appdata permissions, getting the image, updating):
[docs/install-unraid.md](docs/install-unraid.md).

```sh
docker build -t botsu .
docker run -d --name botsu --restart unless-stopped \
  -v /mnt/user/appdata/botsu:/data \
  --env-file /mnt/user/appdata/secrets/bot.env botsu
```

`/data` holds the SQLite database (alert subscriptions and what's already been posted). Without
the volume, a recreated container forgets its subscriptions.

The env file must be root-only (`chmod 600`) and have **no quotes** around values. After editing
it, recreate the container (Edit → Apply); a restart doesn't re-read it.

## Layout

```
crates/botsu-core    platform-neutral core (no platform dependencies)
crates/botsu-discord Discord adapter (only crate using serenity)
crates/botsu-fluxer  Fluxer adapter (only crate using fluxer-rust)
crates/botsu         binary: config, logging, adapter tasks
spikes/              throwaway phase-0 experiments
```

Details (database tables, alert rules, adding a platform or command): [docs/architecture.md](docs/architecture.md).
