# botsu roadmap

Last updated: 2026-11-18. Background and measurements: [handover.md](handover.md).

## Requirements

| | |
|---|---|
| **Platforms** | **Discord and Fluxer are required.** Stoat (formerly Revolt) is a bonus, not required. |
| **Features** | Parity with the Red cogs in use: Tube, streams, cleanup, admin, mod, permissions, audio. Nothing else from Red. |
| **Language** | Rust. Accept churn from young platform libraries; keep it contained in adapter crates. |
| **Memory** | Well under Red + Lavalink's 520 MB–1.2 GB. Target: under 100 MB idle, excluding active ffmpeg/yt-dlp. |
| **Deployment** | Primary: Docker on Unraid. The image must also work for anyone with plain Docker / Docker Compose, with nothing Unraid-specific baked in. A guide for running it natively on Linux (systemd, no containers). |
| **Publishing** | Pushing to `master` on GitHub builds the image and publishes it to Docker Hub. |

## Principles

- **The core never imports a platform crate.** `botsu-core` defines traits; `botsu-fluxer`, `botsu-discord` (and maybe `botsu-stoat`) implement them. A Fluxer update that breaks things is fixed in `botsu-fluxer` only.
- **Capabilities, not assumptions.** Each adapter reports what it supports. A command that a platform can't do answers "not available on Fluxer" instead of failing.
- **One process, many platforms.** One container runs every configured adapter; a platform is enabled by setting its token.
- **Prefix commands everywhere: `$` by default (as with Red), configurable with `BOTSU_PREFIX`. Runs with `!` until cutover so Red is left untouched.** Fluxer has no slash commands in fluxer-rust 0.2. Discord slash commands are optional polish later, not a requirement.
- **Configuration through env vars; state in one data directory** (`/data` in the container) holding the SQLite database. Per-server settings (alert channels, mod-log channel, ...) live in the database and are changed with bot commands, as in Red.
- **Easy wins first, music last.** Each phase ships something usable on both required platforms.

## Phases at a glance

| # | Phase | Size | Ships |
|---|---|---|---|
| 0 | Spikes | S | Answers to the questions that could change the plan |
| 1 | Skeleton on both platforms | S–M | `$ping` / `$help` on Discord and Fluxer, Docker image |
| 2 | Alerts | M | YouTube upload and Twitch live notifications |
| 3 | CI/CD and distribution | S–M | GitHub Actions → Docker Hub on push to `master`; Compose file, Unraid template, native Linux guide |
| 4 | Server tools | S–M | Permissions, cleanup, admin (roles, announcements) |
| 5 | Moderation | M | Kick, ban, temp-ban, timeout, warnings, mod-log cases |
| 6 | Music | L | Play, queue, skip, now-playing on Discord voice and Fluxer (LiveKit) |
| 7 | Cutover | S | Red retired |
| ★ | Stoat (bonus) | M | Stoat chat adapter; music reuses the LiveKit voice adapter |

Phase 0 has no strict order relative to 1: run the spikes early because their answers can move or cut later phases (especially music).

---

## Phase 0: Spikes

Throwaway experiments in `spikes/` (see [spikes/README.md](../spikes/README.md)). Record results there and in `CLAUDE.md`.

- [ ] **Self-hosted Fluxer on Unraid.** Bring up the Docker Compose stack; measure RAM with `docker stats`; note which ports (LiveKit UDP range) must be forwarded.
- [ ] **Fluxer bot voice.** Run fluxer-rust's `voice` example: a bot joins a voice channel and plays a file through LiveKit. *If this fails, music on Fluxer is off the table until Fluxer supports it.*
- [ ] **Discord voice + DAVE.** Join and play a file with songbird. Confirm the songbird version in use supports Discord's DAVE end-to-end voice encryption.
- [ ] **Fluxer API coverage.** Check bulk delete, bans, member timeouts (`communication_disabled_until`) and role edits against a real instance; fluxer-rust has methods for all of them but the server behavior is untested.
- [ ] **Multi-arch libwebrtc.** Check whether LiveKit ships a prebuilt libwebrtc for linux-arm64 (decides whether phase 3 can publish an arm64 image).

**Done when:** each item above has a written answer.

## Phase 1: Skeleton on both platforms

Already done: workspace, `botsu-core`, `botsu-fluxer`, binary, env config, Dockerfile. Remaining:

- [x] **`botsu-discord` crate** on serenity 0.12 (gateway + REST, no cache). Enable the Message Content intent in the Discord developer portal (needed for prefix commands).
- [x] Binary starts every adapter whose token is set (`DISCORD_TOKEN`, `FLUXER_TOKEN`); at least one is required.
- [x] Fluxer startup check: token and API URL verified before connecting; logs `voice_enabled` from `/.well-known/fluxer`.
- [ ] Test `ping` / `help` on the self-hosted Fluxer instance and on the Discord servers: checklist in [testing.md](testing.md). Use a non-`$` prefix while Red is still running.
- [ ] `docker build` works and the image runs on Unraid with the env file from `/mnt/user/appdata/secrets/`.
- [ ] Image runs as a non-root user and works with `--user 99:100` (Unraid's nobody:users) and with a mounted `/data`.

**Done when:** the same container answers `$ping` on Discord and Fluxer at the same time.

## Phase 2: Alerts

The easiest cross-platform feature: "post a message in channel X". Replaces Red's Tube and streams cogs.

- [ ] **Storage:** SQLite in `/data/botsu.db` with migrations embedded in the binary (for example `sqlx` or `rusqlite` + `refinery`). Tables: subscriptions, seen items, per-server settings.
- [ ] **Scheduler:** a polling task in the core with jittered intervals, per-source backoff on errors.
- [ ] **Tube:** poll `https://www.youtube.com/feeds/videos.xml?channel_id=<id>`, no API key. Record seen video IDs; on first subscribe, mark existing videos as seen so the channel isn't flooded.
- [ ] **Streams (Twitch):** Helix API with an app access token (`TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET`). Polling first; EventSub needs a public HTTPS endpoint (Caddy) and can come later.
- [ ] **Streams (YouTube live):** best effort. No clean free API; start with RSS/page heuristics, and drop it if it's unreliable.
- [ ] Commands: `$tube add|remove|list`, `$streams add|remove|list`, choosing the target channel. Admin-only (simple check until phase 4 adds permissions).
- [ ] Alerts can target a channel on either platform; one subscription can post to both.

**Done when:** new uploads and live streams are posted once on Discord and Fluxer, and a restart doesn't repost anything.

## Phase 3: CI/CD and distribution

Get the project building and publishing itself before the bigger features land.

### GitHub
- [ ] Create the GitHub repository; push `master`.
- [ ] **CI workflow (every push and PR):** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Cache cargo and the libwebrtc download (it's large).
- [ ] Branch protection on `master`: CI must pass.
- [ ] Dependabot or Renovate for Cargo and GitHub Actions. Keep `fluxer-rust` and the `livekit` / `webrtc-sys` pins **out** of automatic bumps: they broke the build on day one and need a manual check.

### Docker Hub
- [ ] Create the Docker Hub repository and an access token with read/write scope.
- [ ] GitHub secrets: `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN`.
- [ ] **Release workflow on push to `master`:** `docker/setup-buildx-action`, `docker/login-action`, `docker/metadata-action`, `docker/build-push-action` with GitHub Actions layer cache. Tags: `latest` and `sha-<short>`. On git tags `v*`, also `X.Y.Z` and `X.Y`.
- [ ] `linux/amd64` always. `linux/arm64` only if phase 0 found a prebuilt libwebrtc, built on a native arm64 runner (emulated builds of webrtc would be very slow).
- [ ] OCI labels (source, version, revision) so Docker Hub and Unraid link back to the repo.
- [ ] Sync the README to the Docker Hub description.

### Running it anywhere
- [ ] `deploy/docker-compose.yml`: image from Docker Hub, `env_file`, `/data` volume, `restart: unless-stopped`.
- [ ] `deploy/unraid/botsu.xml`: Unraid template pointing at the Docker Hub image (appdata path, Extra Parameters `--env-file`, `--user 99:100`). Keep a snapshot in the server notes.
- [ ] `docs/install-docker.md`: plain `docker run` and Compose instructions for anyone.
- [ ] `docs/install-linux.md`: native install without containers: download or `cargo build --release`, a `botsu` system user, `/etc/botsu/botsu.env` (mode 600), `/var/lib/botsu` as the data directory, and a hardened systemd unit (`deploy/systemd/botsu.service`). Phase 6 adds the ffmpeg and yt-dlp steps.
- [ ] Decide whether GitHub Releases should also carry a prebuilt Linux binary for the native guide (static linking is tricky with libwebrtc; a glibc build for Debian/Ubuntu is fine).

**Done when:** pushing to `master` publishes a new image to Docker Hub, and Unraid can update to it through the template.

## Phase 4: Server tools

Small features that share one prerequisite: knowing who may run what.

- [ ] **Permissions:** levels `owner` > `admin` > `mod` > `everyone`. Owner from env (`BOTSU_OWNERS`, per platform user IDs); admin/mod roles configured per server with `$set adminrole` / `$set modrole`. Both platforms use the same role-based check. Discord's native slash-command permissions don't apply to prefix commands.
- [ ] **Cleanup:** `$cleanup messages <n>`, `$cleanup user <@user> <n>`, `$cleanup bots <n>`. Respect Discord's limits (bulk delete only < 14 days, 100 per call); fall back to single deletes or report what was skipped. Check Fluxer's equivalent limits.
- [ ] **Admin:** `$addrole`, `$removerole`, `$announce <channel> <text>`.
- [ ] `$help` only lists commands the caller may run.

**Done when:** the Red permissions, cleanup and admin commands in use have equivalents on both platforms.

## Phase 5: Moderation

- [ ] Adapter methods: kick, ban (with reason, optional message deletion), unban, timeout (Discord native; Fluxer `communication_disabled_until`), list bans.
- [ ] **Cases:** every mod action creates a numbered case (who, what, why, when) in SQLite and is posted to a mod-log channel. `$case <n>`, `$reason <n> <text>`.
- [ ] **Warnings:** `$warn`, `$warnings <user>`, `$unwarn`.
- [ ] **Temp-ban:** `$tempban <user> <duration> [reason]`. Expiry stored in the database; the scheduler lifts bans and catches up on startup after downtime.
- [ ] Hierarchy checks: can't act on owners, on users with a higher role, or on the bot itself.
- [ ] Optional: import existing cases from Red's config (only if they're worth keeping).

**Done when:** parity with Red's mod cog as used; a temp-ban survives a container restart and still expires on time.

## Phase 6: Music

The hardest phase: two different voice stacks, YouTube's anti-bot measures, and the least mature library code.

```
search/URL ──► yt-dlp (subprocess) ──► stream URL ──► ffmpeg ──► 48 kHz PCM/Opus ──► voice adapter
                                                                                    ├─ Discord: songbird
                                                                                    └─ Fluxer: LiveKit (also Stoat later)
```

- [ ] **Voice adapter trait** in the core: `join`, `leave`, `play(stream)`, `pause`, `resume`, `stop`; events `track_end`, `disconnected`.
- [ ] **Queue** in the core, per server: play, skip, stop, queue, now-playing, remove, clear, volume. Leave after a configurable idle time.
- [ ] **yt-dlp** as a subprocess, run off the event loop. Resolve the stream URL **when a track starts**, not when it's queued (URLs expire). Search via `ytsearch1:`.
- [ ] **ffmpeg** with `-reconnect 1 -reconnect_streamed 1 -reconnect_delay_max 5`.
- [ ] **yt-dlp updatable without an image rebuild:** install it into `/data` (or a volume) on first start and update with `$ytdlp update` or on a schedule; the image ships only Python and ffmpeg.
- [ ] Optional cookies file in `/data` for YouTube anti-bot checks.
- [ ] **Discord voice:** songbird, with DAVE confirmed in phase 0.
- [ ] **Fluxer voice:** LiveKit. fluxer-rust's `play_music` hardcodes its ffmpeg arguments and keeps its audio source private, so expect to publish our own audio track on `conn.room` (or fork the crate).
- [ ] Update the Docker image and both install guides with ffmpeg and yt-dlp.
- [ ] Measure memory while playing on both platforms; compare with the handover's Lavalink figures.

**Done when:** play, queue and skip work on Discord and Fluxer, playback survives a network hiccup, and yt-dlp can be updated without a new image.

## Phase 7: Cutover

- [ ] Run botsu next to Red on Discord with a different prefix, then give it `$`.
- [ ] Move Tube/streams subscriptions and mod settings over; check nothing is posted twice during the overlap.
- [ ] Stop Red, then remove the Red container and Lavalink. Record memory before and after.
- [ ] Update the server notes (template snapshot, env file location, update procedure).

**Done when:** Red is gone and nobody noticed.

## Bonus: Stoat

Only after phase 6, and only if the community wants it.

- [ ] Pick a Stoat Rust library, or call its REST/WebSocket API directly (its API is not Discord-compatible).
- [ ] `botsu-stoat` chat adapter; capabilities report what Stoat lacks.
- [ ] Music: reuse the LiveKit voice adapter if Stoat bots can join voice (Stoat added LiveKit to self-hosting in February 2026).

---

## Feature × platform matrix

Updated as phases land. ✅ works · 🟡 partial · ❌ not possible · — not started

| Feature | Discord | Fluxer | Stoat (bonus) |
|---|---|---|---|
| Prefix commands | ✅ (untested live) | ✅ (untested live) | — |
| Slash commands | optional | ❌ (not in fluxer-rust 0.2) | — |
| Alerts | — | — | — |
| Permissions / cleanup / admin | — | — | — |
| Moderation | — | — | — |
| Music | — | — (phase 0 decides) | — |

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Fluxer bots can't join voice | No music on Fluxer | Phase 0 spike first; music stays Discord-only until Fluxer supports it |
| fluxer-rust / Fluxer API churn | Builds or features break on update | Exact version pins; all Fluxer code in one crate; fork fluxer-rust if upstream stalls |
| livekit/libwebrtc dependency drift | Build breaks (already happened once) | Commit `Cargo.lock`; pin `livekit*`, `webrtc-sys*`; excluded from automated bumps |
| Discord DAVE not supported by songbird | No Discord music | Phase 0 check; track songbird releases |
| YouTube anti-bot measures | Music and possibly Tube break | yt-dlp updatable at runtime; cookies support |
| No arm64 libwebrtc | amd64-only image | Fine for Unraid; document it |
| Self-hosted Fluxer costs more RAM than botsu saves | The memory win shrinks | Accepted: platform independence is the main goal |
