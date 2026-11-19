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

Checklist and results in [spikes/README.md](../spikes/README.md); also record results in `CLAUDE.md`.

- [x] **Self-hosted Fluxer on Unraid.** Running; `/.well-known/fluxer` reports `voice_enabled=true`, `self_hosted=true`. Uses ~1 GB RAM; LiveKit on ports 7881 and 7882.
- [x] **Fluxer bot voice.** Works (2026-11-19). Built into botsu as `!music` (bot owners): joins the caller's voice channel via LiveKit and plays a test tone. Run it on the deployed container. *If this fails, music on Fluxer is off the table until Fluxer supports it.*
- [x] **Discord voice + DAVE.** Works (2026-11-19) after enabling symphonia's PCM decoder. Same `!music` command, via songbird 0.6.1 (ships DAVE through its `davey` dependency).
- [x] **Voice states:** `!music join` without a channel (works on both, 2026-11-19) needs the platform to tell bots who is in voice. Discord: GUILD_CREATE + VOICE_STATE_UPDATE. Fluxer: GUILD_SYNC / PASSIVE_UPDATES snapshots + VOICE_STATE_UPDATE (fluxer-rust's GUILD_CREATE model drops them).
- [ ] **Fluxer API coverage.** Bulk delete, bans, member timeouts (`communication_disabled_until`) and role edits against the real instance: folded into phase 4, which uses them.
- [x] **Multi-arch libwebrtc.** webrtc-sys-build maps `aarch64` to a prebuilt `linux-arm64` libwebrtc, so an arm64 image is possible with a native arm64 runner. Not needed for Unraid.

**Done when:** each item above has a written answer.

## Phase 1: Skeleton on both platforms

Already done: workspace, `botsu-core`, `botsu-fluxer`, binary, env config, Dockerfile. Remaining:

- [x] **`botsu-discord` crate** on serenity 0.12 (gateway + REST, no cache). Enable the Message Content intent in the Discord developer portal (needed for prefix commands).
- [x] Binary starts every adapter whose token is set (`DISCORD_TOKEN`, `FLUXER_TOKEN`); at least one is required.
- [x] Fluxer startup check: token and API URL verified before connecting; logs `voice_enabled` from `/.well-known/fluxer`.
- [x] Test `ping` on the self-hosted Fluxer instance and on Discord (2026-11-19, prefix `!`, both answered from one process). Rest of the checklist in [testing.md](testing.md) (help, case, reconnect) still to run.
- [x] `docker build` works (2026-11-19: 161 MB image; starts as `--user 99:100` and writes its database to a mounted `/data`).
- [ ] Runs on Unraid: [install-unraid.md](install-unraid.md). Image published manually as `caaarrrlll/botsu:latest` (2026-11-19) until the phase 3 workflow does it.
- [x] Image runs as a non-root user and works with `--user 99:100` (Unraid's nobody:users) and with a mounted `/data`.

**Done when:** the same container answers `$ping` on Discord and Fluxer at the same time.

## Phase 2: Alerts

The easiest cross-platform feature: "post a message in channel X". Replaces Red's Tube and streams cogs.

- [x] **Storage:** SQLite (`rusqlite`, bundled) in `<BOTSU_DATA_DIR>/botsu.db` (`/data` in Docker), migrations built into the binary (`storage.rs`). Tables: subscriptions, seen items, source state. Per-server settings come with phase 4.
- [x] **Scheduler:** one polling task per source type, ±10% jitter, intervals configurable (`YOUTUBE_POLL_SECONDS`, `TWITCH_POLL_SECONDS`). Per-source error backoff not done yet: a failing feed is just logged and retried next cycle.
- [x] **Tube:** RSS feed, no API key. Accepts a channel link, `@handle` or channel ID. Everything in the feed at subscribe time counts as seen; uploads older than 7 days are never posted.
- [x] **Streams (Twitch):** Helix with an app access token (auto-renewed). A stream restarting within 15 minutes isn't announced twice. EventSub still possible later.
- [x] **Streams (YouTube live):** via the Data API (`YOUTUBE_API_KEY`): live streams post as "is live", scheduled streams/premieres wait until they start. RSS stays the detector (no quota).
- [x] **Shorts:** detected from the feed's `/shorts/` links; off by default per subscription, `tube shorts <name> on|off`.
- [x] Commands: `tube add|remove|list`, `twitch add|remove|list` (`[#channel]` picks the target; the bot test-posts there first). Add/remove limited to `BOTSU_OWNERS` until phase 4.
- [x] Alerts target channels on either platform; the same source can post to Discord and Fluxer channels.
- [ ] Live test: a real upload and a real stream posted once on both platforms, nothing reposted after a restart.
- [x] `tube import` / `twitch import`: create subscriptions from a pasted list (Red's `tube list` format, Twitch names with `and`/commas, `youtube:`/`twitch:` sections).
- [ ] Optional, Red parity: custom alert message and role mention per subscription.
- [ ] Optional: `tube remove <name> #channel` to remove a source from one channel only.
- [ ] Optional: record the Fluxer instance on subscriptions, if the bot ever needs to move between instances with the same database (see architecture.md, `subscriptions`).

**Done when:** new uploads and live streams are posted once on Discord and Fluxer, and a restart doesn't repost anything.

## Phase 3: CI/CD and distribution

Get the project building and publishing itself before the bigger features land.

### GitHub
- [x] Create the GitHub repository; push `master`.
- [x] **CI workflow** (`.github/workflows/ci.yml`, every push and PR): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test`. `Swatinem/rust-cache` caches cargo and the libwebrtc download.
- [ ] Branch protection on `master`: CI must pass. Skipped for now: with one developer pushing straight to `master` it only adds friction. Revisit if others contribute.
- [x] Dependabot (`.github/dependabot.yml`): weekly Cargo (minor/patch grouped), GitHub Actions and Docker base images. `fluxer-rust`, `livekit*`, `webrtc-sys*` and `libwebrtc` are ignored and bumped by hand.

### Docker Hub
- [x] Docker Hub repository `caaarrrlll/botsu` (public; created by the first manual push on 2026-11-19).
- [x] Docker Hub access token (Read & Write) stored as the GitHub secret `DOCKERHUB_TOKEN` (2026-11-19; first automatic publish: run 44495320555, `sha-a1e8a85`). The username is in the workflow.
- [x] **Release job** in the same workflow, after the checks pass, only on `master` and `v*` tags: buildx + metadata + build-push with the GitHub Actions layer cache. Tags: `latest` and `sha-<short>` on `master`; `X.Y.Z` and `X.Y` on tags `vX.Y.Z`.
- [x] `linux/amd64`. `linux/arm64` only if phase 0 finds a prebuilt libwebrtc, built on a native arm64 runner (emulated webrtc builds would be very slow).
- [x] OCI labels (source, revision, version) via `docker/metadata-action`, so Docker Hub and Unraid link back to the repo.
- [ ] Sync the README to the Docker Hub description (needs a token with admin scope; optional).
- [ ] Faster image builds: the Dockerfile's cargo cache mount isn't kept between CI runs, so each image build compiles from scratch (~10–15 min). Fix with `cargo-chef` layers if it gets annoying.

### Running it anywhere
- [x] `deploy/docker-compose.yml`: image from Docker Hub, `./data` volume, PUID/PGID, optional `env_file`, `restart: unless-stopped`.
- [x] `deploy/unraid/my-botsu.xml`: pulls `caaarrrlll/botsu:latest`; data folder, settings as fields (tokens masked) or in the auto-created `botsu.env`, PUID/PGID. Icon is a placeholder (`assets/monkasS.png`).
- [ ] Later (user): the template on the live server still has the old "Bot owners" description (says owners add/remove alerts, suggests `!tube add x` for the ID). Cosmetic. Refresh by re-downloading only while all fields are blank; Unraid saves field values into the same file, so re-downloading would wipe them. Otherwise edit the line by hand.
- [x] `docs/install-docker.md`: `docker run` and Compose for anyone, tags, settings, updating and rolling back.
- [x] **Invite links in the startup log**, one per connected platform: Discord from `ready.application.id`, Fluxer from the bot's user ID plus the discovered web app address. Permissions in `botsu_core::INVITE_PERMISSIONS` (now including Embed Links for alert previews). `*_APPLICATION_ID` in `.env.example` are kept as optional, unused reference values.
- [x] `docs/install-linux.md` + `deploy/systemd/botsu.service`: build from source, `/etc/botsu/botsu.env` (600), `DynamicUser` + `StateDirectory` (`/var/lib/botsu`), hardened sandbox (`systemd-analyze security`: 3.2 OK). Phase 6 adds ffmpeg and yt-dlp.
- [ ] Decide whether GitHub Releases should also carry a prebuilt Linux binary for the native guide (a glibc build for Debian/Ubuntu is fine; static linking is tricky with libwebrtc).

**Done when:** pushing to `master` publishes a new image to Docker Hub, and Unraid can update to it through the template.

## Phase 4: Server tools

Small features that share one prerequisite: knowing who may run what.

- [x] **Permissions** (`permissions.rs`): bot owner (`BOTSU_OWNERS`) > server owner > admin > mod > everyone. Admin = a role set with `set adminrole add`, or any role with Administrator (including `@everyone`), so it works without setup. Mod = a role set with `set modrole add`. Stored per platform and server (migration 3, `guild_roles`). Each command declares a minimum level; the bot only looks levels up (a few API calls) for commands that need one. Lookup failures count as "everyone".
- [x] **Cleanup** (mods): `cleanup messages <n>`, `cleanup user <@user> <n>`, `cleanup bots <n>` (bot messages and prefix commands). Up to 1000, scanning up to 1000 messages; stops at 14 days old (bulk-delete limit, applied on both platforms); batches of 100 with one-by-one fallback; the confirmation deletes itself after 5 s.
- [x] **Admin** (admins): `addrole <@user> <role>`, `removerole <@user> <role>` (role by mention, ID or name; roles with Administrator only by the server owner), `announce <#channel> <text>`.
- [x] `help` only lists commands the caller may run.
- [x] Channel IDs from other servers are refused everywhere a channel is given (`announce`, `tube/twitch add` targets, import channel IDs).
- [x] Invite permissions add Manage Messages and Manage Roles.
- [ ] Live test on Discord and Fluxer (Fluxer's bulk-delete limits and role API are untested): checklist in [testing.md](testing.md) section 6.

**Done when:** the Red permissions, cleanup and admin commands in use have equivalents on both platforms.

## Phase 5: Moderation

- [x] Adapter methods: kick, ban (with reason, optional message deletion), unban, timeout (Discord native; Fluxer `communication_disabled_until`), list bans. Reasons go to the audit log (Fluxer: `X-Audit-Log-Reason`, sent by botsu itself since fluxer-rust's helpers can't).
- [x] **Cases:** every mod action creates a numbered case (who, what, why, when) in SQLite (migration 4) and is posted to a mod-log channel (`set modlog #channel`). `case <n>`, `cases <user>`, `reason <n> <text>`.
- [x] **Warnings:** `warn`, `warnings [user]` (own for anyone, others for mods), `unwarn <case>`.
- [x] **Temp-ban:** `tempban <user> <duration> [reason]`. Expiry stored in the database; a task checks every 30 s from startup, so it catches up after downtime. Plus `timeout`/`untimeout` (max 28 days) and `bans`.
- [x] Hierarchy checks: can't act on yourself, the server owner or the bot; the target must be below the moderator in botsu's levels and in role rank.
- [ ] Live test on Discord and Fluxer: checklist in [testing.md](testing.md) section 7.
- [ ] Optional: import existing cases from Red's config (only if they're worth keeping).
- [ ] Optional: DM the user before a kick/ban, and `$mute` via a role on platforms without timeouts.

**Done when:** parity with Red's mod cog as used; a temp-ban survives a container restart and still expires on time.

## Phase 6: Music

The hardest phase: two different voice stacks, YouTube's anti-bot measures, and the least mature library code.

```
search/URL ──► yt-dlp (subprocess) ──► stream URL ──► ffmpeg ──► 48 kHz PCM/Opus ──► voice adapter
                                                                                    ├─ Discord: songbird
                                                                                    └─ Fluxer: LiveKit (also Stoat later)
```

- [x] **Voice adapter trait** in the core: `join`, `leave`, `play(source)`, `stop`, `is_playing`, `is_connected`. Track end is detected by polling `is_playing` once a second (works the same on both platforms). No pause/resume yet (fluxer-rust's playback can't pause).
- [x] **Queue** in the core, per server (`music.rs`): `music play|skip|stop|queue|now|remove|clear|shuffle|join|leave`. Playlists up to 100 tracks; queue up to 500. Anyone in the bot's voice channel controls it, mods from anywhere. Leaves after 10 minutes without sound.
- [ ] Volume (Discord only: songbird can, fluxer-rust's `play_music` can't).
- [x] **yt-dlp** as a subprocess (`ytdlp.rs`), off the event loop. Lookup with `--flat-playlist -J` when queued (title, length, page URL); the stream URL is resolved **when a track starts**. Search via `ytsearch1:`.
- [x] **ffmpeg** with `-reconnect 1 -reconnect_streamed 1 -reconnect_delay_max 5` (Discord; Fluxer's `play_music` can't take them yet).
- [x] **yt-dlp updatable without an image rebuild:** botsu downloads the standalone build into `/data/bin` on first start, runs `yt-dlp -U` daily and on `music update` (bot owners). `BOTSU_YTDLP` uses another copy. The image ships Deno (yt-dlp needs a JavaScript runtime for YouTube since 2025.11).
- [ ] Live test on Discord and Fluxer: checklist in [testing.md](testing.md) section 8.
- [ ] Optional cookies file in `/data` for YouTube anti-bot checks (only if YouTube starts refusing the server's IP).
- [x] **Discord voice:** songbird, with DAVE confirmed in phase 0.
- [ ] **Fluxer voice:** uses fluxer-rust's `play_music` for now. Its hardcoded ffmpeg arguments have no reconnect flags, so long tracks may cut off on a network hiccup; if they do, publish our own audio track on `conn.room` (or fork the crate).
- [x] Docker image ships Deno (yt-dlp downloads itself); the native Linux guide lists ffmpeg and Deno.
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
| Prefix commands | ✅ | ✅ (self-hosted) | — |
| Slash commands | optional | ❌ (not in fluxer-rust 0.2) | — |
| Alerts | ✅ (code, live test pending) | ✅ (code, live test pending) | — |
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
