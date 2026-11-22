# botsu

A lean, self-hosted chat bot in Rust for **Discord** and **[Fluxer](https://fluxer.app)**, run
from one platform-neutral core. It replaced a Red-DiscordBot instance (with Lavalink) on
2026-11-22, using a fraction of the memory. Plan: [docs/roadmap.md](docs/roadmap.md). How it
works inside (crates, database tables, alert rules, extending):
[docs/architecture.md](docs/architecture.md). Background: [docs/handover.md](docs/handover.md).

## What it does

- **Alerts:** YouTube uploads and live streams, Twitch live streams, posted as an embed with a
  large image; custom text and a role or user mention per alert.
- **Moderation:** warn, kick, ban, temp-ban, timeout, mute, numbered cases, a mod log, optional
  DMs to members.
- **Server tools:** admin and mod roles, cleanup, add/remove roles, announcements, a per-server
  prefix, custom commands, and exporting/importing a server's setup between servers or platforms.
- **Music:** YouTube (and more, via yt-dlp) in voice channels: queue, playlists, pause, volume.
- `help` lists what each member may run; `invite` gives the invite links; `feedback` links to
  the issue tracker.

## Status

Running in production on Discord and a self-hosted Fluxer instance since 2026-11-22. First
release: [v0.1.0](https://github.com/Caaarrrlll/botsu/releases/tag/v0.1.0).

| Area | State |
|---|---|
| Alerts (YouTube, Twitch) | live on both platforms |
| Music | live on both platforms |
| Server tools | built; `cleanup` live-tested, the rest pending |
| Moderation | built; live test pending |
| Custom commands, per-server prefix, settings export/import | built; live test pending |
| CI/CD | every push to `master` publishes the Docker image; `vX.Y.Z` tags publish release binaries |
| Cutover from Red | done 2026-11-22; Red's removal pending |

Memory: about 34 MiB idle, about 112 MiB while playing music on Discord
([details](docs/resource-usage.md)). Live test checklists: [docs/testing.md](docs/testing.md).

## Run locally

```sh
cp .env.example .env   # fill in DISCORD_TOKEN and/or FLUXER_TOKEN
set -a; . ./.env; set +a
cargo run -p botsu
```

Then type `$ping` in a channel the bot can see. If another bot on the same servers already uses
`$`, set `BOTSU_PREFIX` (for example `!`).

The first build is slow: `fluxer-rust` pulls in LiveKit, which downloads and links libwebrtc.

## Configuration

All configuration is environment variables; see [.env.example](.env.example).

| Variable | Required | Default |
|---|---|---|
| `DISCORD_TOKEN` | one of the two | enables Discord |
| `FLUXER_TOKEN` | one of the two | enables Fluxer |
| `FLUXER_URL` | no | your self-hosted instance's web address; API URL is discovered. Unset = fluxer.app |
| `FLUXER_API_URL` | no | explicit API base with `/v1`; overrides `FLUXER_URL` |
| `BOTSU_PREFIX` | no | `$`; a server can set its own with `setprefix` (mods) |
| `BOTSU_OWNERS` | recommended | user IDs (any platform) of the bot's owners: they outrank everyone and alone may import alert lists, toggle Shorts and run the music test tools |
| `BOTSU_INVITE_ADMIN` | no | `false`; `true` makes the invite links ask for Administrator |
| `YOUTUBE_API_KEY` | recommended | YouTube Data API v3 key: @handles, live/upcoming stream detection |
| `TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET` | for Twitch alerts | from dev.twitch.tv; both or neither |
| `BOTSU_DATA_DIR` | no | `data` (`/data` in Docker): SQLite database |
| `YOUTUBE_POLL_SECONDS`, `TWITCH_POLL_SECONDS` | no | `300`, `120` |
| `BOTSU_YTDLP` | no | botsu's own yt-dlp in the data folder, updated daily; set a path to use another |
| `BOTSU_LOG` | no | log levels to print, any combination of `ERROR,WARN,INFO,DEBUG,TRACE` or `ALL`; blank = `ERROR,WARN,INFO` |
| `RUST_LOG` | no | expert override in tracing's filter syntax; wins over `BOTSU_LOG` from the same place |

Getting tokens and testing against live servers: [docs/testing.md](docs/testing.md).

## Docker / Unraid

Image: `caaarrrlll/botsu:latest` on Docker Hub, built and published by GitHub Actions on every push
to `master` once the checks pass (`sha-<commit>` tags too, for rollbacks).
Guides: [Unraid](docs/install-unraid.md) (template) · [any Docker host](docs/install-docker.md)
(`docker run` / Compose) · [native Linux](docs/install-linux.md) (systemd; `.deb`, `.tar.gz` or
from source on Debian/Ubuntu, Fedora, Arch) · [Windows](docs/install-windows.md). Prebuilt
binaries are on the [releases page](https://github.com/Caaarrrlll/botsu/releases) for each
`vX.Y.Z` tag.

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
spikes/              throwaway experiments
```

Details (database tables, alert rules, adding a platform or command): [docs/architecture.md](docs/architecture.md).

## License

MIT: free to use, change and share, for any purpose. See [LICENSE](LICENSE).
