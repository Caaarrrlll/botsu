# botsu

A lean, self-hosted chat bot in Rust, replacing a Red-DiscordBot instance. It runs on
**Discord** and **[Fluxer](https://fluxer.app)** from one platform-neutral core (Stoat is a
possible bonus). Plan: [docs/roadmap.md](docs/roadmap.md). Background: [docs/handover.md](docs/handover.md).

## Status

| Phase | State |
|---|---|
| 0. Spikes (voice on Fluxer, Discord DAVE) | open, see [spikes/](spikes/README.md) |
| 1. Skeleton on Discord + Fluxer | code done; live testing in progress ([docs/testing.md](docs/testing.md)) |
| 2. Alerts (YouTube uploads, Twitch live) | |
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
| `FLUXER_API_URL` | no | `https://api.fluxer.app/v1` (set for a self-hosted instance) |
| `BOTSU_PREFIX` | no | `$` |

Getting tokens and testing against live servers: [docs/testing.md](docs/testing.md).
| `RUST_LOG` | no | `info,botsu=debug` |

## Docker / Unraid

```sh
docker build -t botsu .
docker run -d --name botsu --restart unless-stopped \
  --env-file /mnt/user/appdata/secrets/bot.env botsu
```

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
