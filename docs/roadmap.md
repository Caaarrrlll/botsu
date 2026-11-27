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
- [ ] **Fluxer API coverage.** Bulk delete, bans, member timeouts (`communication_disabled_until`) and role edits against the real instance. Docs checked 2026-11-20: botsu's calls match; Fluxer has no 14-day bulk-delete limit, allows 365-day timeouts and native temporary bans. Live check: `api_spike` example ([spikes/README.md](../spikes/README.md#fluxer-api-spike)). First run 2026-11-20: bulk delete, roles, embeds, file upload, channel list pass; timeouts/kick/ban/audit reasons need a second account; roles may share positions.
- [x] **Multi-arch libwebrtc.** webrtc-sys-build maps `aarch64` to a prebuilt `linux-arm64` libwebrtc, so an arm64 image is possible with a native arm64 runner. Not needed for Unraid.

**Done when:** each item above has a written answer.

## Phase 1: Skeleton on both platforms

Already done: workspace, `botsu-core`, `botsu-fluxer`, binary, env config, Dockerfile. Remaining:

- [x] **`botsu-discord` crate** on serenity 0.12 (gateway + REST, no cache). Enable the Message Content intent in the Discord developer portal (needed for prefix commands).
- [x] Binary starts every adapter whose token is set (`DISCORD_TOKEN`, `FLUXER_TOKEN`); at least one is required.
- [x] Fluxer startup check: token and API URL verified before connecting; logs `voice_enabled` from `/.well-known/fluxer`.
- [x] Test `ping` on the self-hosted Fluxer instance and on Discord (2026-11-19, prefix `!`, both answered from one process). Rest of the checklist in [testing.md](testing.md) (help, case, reconnect) still to run.
- [x] `docker build` works (2026-11-19: 161 MB image; starts as `--user 99:100` and writes its database to a mounted `/data`).
- [x] Runs on Unraid: [install-unraid.md](install-unraid.md). The image is published by CI (phase 3).
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
- [x] Live test: YouTube uploads, YouTube live and Twitch live posted on Discord and Fluxer; no reposts after a restart (2026-11-20). Left for later: a restart while a stream is live, a scheduled premiere.
- [x] `tube import` / `twitch import`: create subscriptions from a pasted list (Red's `tube list` format, Twitch names with `and`/commas, `youtube:`/`twitch:` sections).
- [x] Red parity: custom alert text (`tube|twitch message`, placeholders) and a role / @everyone / @here mention per subscription (`tube|twitch mention`), migration 5. Live test in testing.md section 5.
- [x] `tube|twitch remove <name> #channel` removes a source from one channel only.
- [x] Alert embeds: botsu posts its own embed with a large image (YouTube thumbnail, Twitch live frame), `set embeds on|off` per server (on by default), plain-text fallback. Live test in testing.md section 5.
- [ ] Parked: Twitch EventSub over WebSocket (live alerts within seconds; needs a user token). Same status as YouTube cookies.
- [x] Free games (to keep) on Epic, Steam and GOG, like Patchbot: `freegames add [epic steam gog] [#channel]` with remove/list/message/mention, `AlertKind::FreeGames` (no migration; `source_id` = store), GamerPower API (free, no key) polled every 30 min, embed with price and end date. Live test in testing.md section 12. Epic's own `freeGamesPromotions` feed is the fallback if GamerPower turns out unreliable.
- [ ] Later, to explore (not required): relay Discord announcement channels. Goal: give botsu a link like `discord.com/channels/<server>/<channel>` and have that channel's posts appear in one of ours, on Discord or Fluxer.
  - **Limit:** a bot only sees channels in servers it was invited to, so it can't read a link to someone else's server directly. Reading it with a user account (self-bot) breaks Discord's terms; not an option.
  - **Discord → Discord needs no botsu code:** in the source server, an announcement channel has a **Follow** button. It adds a webhook to one of our channels, and Discord copies every post there. A member with Manage Webhooks in our server can do it. The API version (`POST /channels/{id}/followers`) also needs the bot to see the source channel, so it has the same limit.
  - **What botsu could add:** `relay add #source-channel <platform> #target` copies new messages from a channel botsu can read (for example one that receives followed announcements) to a channel on another platform, mainly Discord → Fluxer. It would use the gateway messages botsu already receives (including webhook posts; Message Content intent is already on). It posts the text, embeds and attachments, with the original author or server as a header. Store it in a `relays` table (migration, so `experimental` first). Avoid loops: never relay botsu's own messages.
  - **Check first:** whether followed-channel posts reach bots with their full content and embeds, and how Fluxer shows relayed embeds and attachments.
- [ ] Later, to explore (not required): game and software update alerts, like Patchbot. Nearly every source is a feed: Steam news `store.steampowered.com/feeds/news/app/<appid>/`, GitHub releases `github.com/<owner>/<repo>/releases.atom`, any RSS/Atom URL. Shape: `AlertKind::Feed` in the existing `subscriptions` table (no migration; `source_id` = feed URL), a poller beside YouTube/Twitch (~15 min, dedup by entry ID, baseline on add), delivery through `deliver` (embeds, `message`, `mention` work unchanged; Steam header image), `!feed add <steam game | github:owner/repo | URL> [#channel]` with list/remove/message/mention, `"feed"` in `transfer::kind_of`. Parse with the `feed-rs` crate (RSS 2.0, which Steam uses, isn't covered by the YouTube Atom parser). About the size of Twitch alerts.
- [x] Several Fluxer instances at once (official `FLUXER_OFFICIAL_TOKEN` plus self-hosted `FLUXER_URL`/`FLUXER_TOKEN` lists): each instance is its own platform with a fixed storage key, which also records the instance on subscriptions. Tested live on `experimental` and released to `latest` (2026-11-27); testing.md section 11.

**Done when:** new uploads and live streams are posted once on Discord and Fluxer, and a restart doesn't repost anything.

## Phase 3: CI/CD and distribution

Get the project building and publishing itself before the bigger features land.

### GitHub
- [x] Create the GitHub repository; push `master`.
- [x] **CI workflow** (`.github/workflows/ci.yml`, every push and PR): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test`. `Swatinem/rust-cache` caches cargo and the libwebrtc download.
- [x] Release channels: `master` → `caaarrrlll/botsu:latest`, `experimental` → `caaarrrlll/botsu:experimental`; which change goes where: [release-channels.md](release-channels.md).
- [ ] Branch protection on `master`: CI must pass. Skipped for now: with one developer pushing straight to `master` it only adds friction. Revisit if others contribute.
- [x] Dependabot (`.github/dependabot.yml`): weekly Cargo (minor/patch grouped), GitHub Actions and Docker base images. `fluxer-rust`, `livekit*`, `webrtc-sys*` and `libwebrtc` are ignored and bumped by hand.

### Docker Hub
- [x] Docker Hub repository `caaarrrlll/botsu` (public; created by the first manual push on 2026-11-19).
- [x] Docker Hub access token (Read & Write) stored as the GitHub secret `DOCKERHUB_TOKEN` (2026-11-19; first automatic publish: run 44495320555, `sha-a1e8a85`). The username is in the workflow.
- [x] **Release job** in the same workflow, after the checks pass, only on `master` and `v*` tags: buildx + metadata + build-push with the GitHub Actions layer cache. Tags: `latest` and `sha-<short>` on `master`; `X.Y.Z` and `X.Y` on tags `vX.Y.Z`.
- [x] `linux/amd64`. `linux/arm64` only if phase 0 finds a prebuilt libwebrtc, built on a native arm64 runner (emulated webrtc builds would be very slow).
- [x] OCI labels (source, revision, version) via `docker/metadata-action`, so Docker Hub and Unraid link back to the repo.
- [x] Docker Hub description: updated by hand from README.md when it matters (decided 2026-11-20). Automating it would need a second token with Read, Write & Delete scope (Docker Hub refuses description edits from Read & Write tokens); not worth it.
- [x] Faster image builds: `cargo-chef` stages in the Dockerfile put the dependency build (and the libwebrtc download) in its own layer, which the GitHub Actions layer cache keeps; only Cargo.toml/Cargo.lock/`vendor/` changes rebuild it. Pull requests build the image without publishing it, so Dockerfile changes are tested before `master`.

### Running it anywhere
- [x] `deploy/docker-compose.yml`: image from Docker Hub, `./data` volume, PUID/PGID, optional `env_file`, `restart: unless-stopped`.
- [x] `deploy/unraid/my-botsu.xml`: pulls `caaarrrlll/botsu:latest`; data folder, settings as fields (tokens masked) or in the auto-created `botsu.env`, PUID/PGID. Icon is a placeholder (`assets/monkasS.png`).
- [x] Migrated (2026-11-20): migrate the live server to the current template (`deploy/unraid/my-botsu.xml`, up to date 2026-11-20): new overview and Bot owners text, new fields "Invite as admin" (`BOTSU_INVITE_ADMIN`) and, under advanced, "yt-dlp path" (`BOTSU_YTDLP`). Unraid saves field values in the same file, so note them before replacing it.
- [x] `docs/install-docker.md`: `docker run` and Compose for anyone, tags, settings, updating and rolling back.
- [x] **Invite links in the startup log**, one per connected platform: Discord from `ready.application.id`, Fluxer from the bot's user ID plus the discovered web app address. Permissions in `botsu_core::INVITE_PERMISSIONS` (now including Embed Links for alert previews); `BOTSU_INVITE_ADMIN=true` requests Administrator instead (off by default). `*_APPLICATION_ID` in `.env.example` are kept as optional, unused reference values.
- [x] `invite` command: lists the same links, one per connected platform. Each adapter registers its link with `bot.register_invite` when it connects, so a new platform (Stoat) shows up by registering its own.
- [x] `feedback` command (aliases `bug`, `feature`, `issue`): links to the GitHub issues page (`command::ISSUES_URL`).
- [x] Red parity: custom commands (`cc create|edit|delete|list|show`, Red's CustomCommands), migration 7, in export/import. Not built: Red's random responses and cooldowns (add if Red's CustomCommands data shows they're used).
- [x] Red parity: per-server prefix (`setprefix`, mods).
- [x] `docs/install-linux.md` + `deploy/systemd/botsu.service`: build from source, `/etc/botsu/botsu.env` (600), `DynamicUser` + `StateDirectory` (`/var/lib/botsu`), hardened sandbox (`systemd-analyze security`: 3.2 OK). Phase 6 adds ffmpeg and yt-dlp.
- [x] GitHub Releases carry prebuilt binaries (`.github/workflows/release.yml`, on `v*` tags; run by hand to only build): Linux x86_64 `.tar.gz` and `.deb` (built on Ubuntu 22.04, glibc 2.35+, so Fedora and Arch too) and Windows x86_64 `.zip`. Guides: install-linux.md (`.deb`, `.tar.gz`, source builds for Debian/Ubuntu, Fedora, Arch), install-windows.md.
- [ ] First release: tag `v0.1.0`. Dry run passed 2026-11-20 (run 44696458540: .tar.gz, .deb, .zip; the Windows build starts and creates `dataotsu.env`).

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
- [x] DM the member before an action (`set moddm on`, off by default) and `mute`/`unmute` with a mute role (`set muterole`), timed mutes lifted by the temp-ban task (migration 6); without a mute role, `mute` is a timeout. Live test in testing.md section 7.

**Done when:** parity with Red's mod cog as used; a temp-ban survives a container restart and still expires on time.

## Phase 6: Music

The hardest phase: two different voice stacks, YouTube's anti-bot measures, and the least mature library code.

```
search/URL ──► yt-dlp (subprocess) ──► stream URL ──► ffmpeg ──► 48 kHz PCM/Opus ──► voice adapter
                                                                                    ├─ Discord: songbird
                                                                                    └─ Fluxer: LiveKit (also Stoat later)
```

- [x] **Voice adapter trait** in the core: `join`, `leave`, `play(source)`, `stop`, `is_playing`, `is_connected`. Track end is detected by polling `is_playing` once a second (works the same on both platforms). `set_paused` pauses (songbird's pause on Discord; a pause flag in the vendored Fluxer player).
- [x] **Queue** in the core, per server (`music.rs`): `music play|skip|stop|queue|now|remove|clear|shuffle|join|leave`. Playlists up to 100 tracks; queue up to 500. Anyone in the bot's voice channel controls it, mods from anywhere. Leaves after 10 minutes without sound.
- [x] **Pause/resume:** `music pause|resume`. A paused bot still leaves after 10 minutes. Confirmed on Fluxer 2026-11-19.
- [x] **Volume:** `music volume [0-150]`, saved per server (`guild_settings`). Discord: songbird's track volume; Fluxer: the vendored `play_input` scales each frame. Live test in testing.md section 8.
- [x] **yt-dlp** as a subprocess (`ytdlp.rs`), off the event loop. Lookup with `--flat-playlist -J` when queued (title, length, page URL); the audio is fetched **when a track starts** (`yt-dlp -o -` piped into ffmpeg; YouTube often refuses bare stream URLs). Search via `ytsearch1:`.
- [x] **ffmpeg** reads yt-dlp's output on stdin on both platforms (Fluxer via the vendored `play_input` patch); `-reconnect` flags are added for http inputs.
- [x] **yt-dlp updatable without an image rebuild:** botsu downloads the standalone build into `/data/bin` on first start, runs `yt-dlp -U` daily and on `music update` (bot owners). `BOTSU_YTDLP` uses another copy. The image ships Deno (yt-dlp needs a JavaScript runtime for YouTube since 2025.11).
- [ ] Live test on Discord and Fluxer: checklist in [testing.md](testing.md) section 8. Fluxer playback confirmed 2026-11-19, Discord 2026-11-22 (112.5 MiB, 0.06% CPU while playing). Seen once: yt-dlp `HTTP Error 403` on a Fluxer track while Discord was also playing; yt-dlp was current and a retry worked, so a one-off. If it repeats: retry once on 403, then the cookies file.
- [ ] Parked: optional cookies file in `/data` for YouTube anti-bot checks (only if YouTube starts refusing the server's IP).
- [x] **Discord voice:** songbird, with DAVE confirmed in phase 0.
- [x] **Fluxer voice:** botsu's patched fluxer-rust (`vendor/`) adds `play_input`: ffmpeg with reconnect flags, reading yt-dlp's output from stdin, errors returned to botsu instead of posted to a channel. Both platforms now pipe `yt-dlp -o -` into ffmpeg.
- [x] Docker image ships Deno (yt-dlp downloads itself); the native Linux guide lists ffmpeg and Deno.
- [x] Memory: about 60 MB idle (target: under 100 MB) and about 230 MB while playing, for the container (2026-11-19), against Red + Lavalink's 520 MB to 1.2 GB. After 24 h mostly idle (2026-11-22): botsu 34 MiB, Red 376 MiB (Red's container alone, Lavalink not counted), both 0% CPU. Playing (2026-11-22): Discord alone 112.5 MiB / 0.06% CPU; Discord and Fluxer at once 251.8 MiB / 0.17% CPU. All measurements and what the memory is made of: [resource-usage.md](resource-usage.md).

**Done when:** play, queue and skip work on Discord and Fluxer, playback survives a network hiccup, and yt-dlp can be updated without a new image.

## Phase 7: Cutover

Runbook: [cutover.md](cutover.md).

- [x] Run botsu next to Red on Discord with a different prefix (`!`, since phase 2).
- [x] Give it `$` (cutover.md step 2), 2026-11-22: template Command prefix cleared (blank = `$`). Red turned off; kicked from the server that had nothing configured. A `BOTSU_PREFIX=!` left in botsu.env would still win over the blank field.
- [x] Move Tube/streams subscriptions and mod settings over (2026-11-22, all three Discord servers; from Red's cog settings: 5 Tube subscriptions, 5 Twitch alerts, volume 60. Red had no mod log, cases, custom commands, admin/mod roles or prefixes to move).
- [ ] Stop Red, then remove the Red container and Lavalink. Record memory before and after. Red stopped 2026-11-22 and kept a week as rollback (idle: Red 376 MiB, botsu 34 MiB). After the week: delete Red's Discord application (removes it from all servers, no `$leave` needed; irreversible), then the container, Lavalink and their appdata.
- [ ] Update the server notes (template snapshot, env file location, update procedure).
- [x] Export/import a server's setup between any two servers, Discord or Fluxer (`admin export`, `admin import` + file → preview → `confirm`; `--replace`, `map old = new`). No user data. `transfer.rs`; plan and details: [settings-migration.md](settings-migration.md). Live test: testing.md section 9.
- [ ] Optional, later: `import --create-channels`, and `import from <platform> <server>` within one botsu.
- [x] Server structure copy, Discord → Fluxer (`admin structure export|import|confirm|cancel`, results by DM): roles, categories, channels, per-role channel overrides, name/icon/banner. No members. `structure.rs`; live test: testing.md section 10.

**Done when:** Red is gone and nobody noticed.

## Later: XP and levels (planned, needs fine-tuning before building)

Members earn XP by chatting and using the bot, and level up with an announcement. **Opt-in per
server**: off until an admin or mod turns it on.

- [ ] **Turning it on:** `xp on|off` (mods and admins). Level-up announcements go to the channel
  where it happened, to a set channel (`xp channel #channel`), or nowhere (`xp announce off`).
- [ ] **XP for chatting:** a flat amount per message: **1 XP per message, a fifteenth of level 1**
  (level 1 at 15 XP, so 15 messages).
- [ ] **XP for commands:** only commands everyone may use (nothing that needs mod or admin).
  Draft amounts, relative to level 1 (15 XP):

  | Command | XP | Share of level 1 |
  |---|---|---|
  | `music play` | 4 | ~25% |
  | other `music` controls (skip, pause, queue, …) | 2 | ~13% |
  | using a custom command (`<prefix><name>`) | 2 | ~13% |
  | `tube list`, `twitch list`, `invite`, `feedback`, `cc list` | 1 | ~7% |

- [ ] **Level curve like Ark (Survival Evolved / Ascended):** small steps early, steeper later.
  Draft formula: XP from level L to L+1 = `round(15 × L^1.6)`: L1→2 needs 15, L2→3 45, L5→6 196,
  L10→11 597, L20→21 1811. Tune against real activity before shipping.
- [ ] **Anti-farming:** chat XP at most once per user per 60 seconds; command XP at most once per
  command per user per minute; no XP for bot accounts, DMs or the bot's own messages.
- [ ] **Data (basic, per server):** a table like `xp_stats(platform, guild_id, user_id, xp, level,
  messages, music_commands, cc_uses, other_commands, last_xp_at)`, for example "user x: 15
  messages, 3 music commands, 1 cc". This is **user data**: not in settings export/import, and
  removed with `xp reset @user` (mods). Turning XP off keeps the data until reset.
- [ ] **Commands for members:** `rank [@user]` (level, XP to next level, the counts above) and
  `leaderboard` (top 10 in this server).

## Later: rate limits for members (planned)

Stops a member from flooding a server with alerts. Mods, admins, the server owner and bot owners
are exempt.

- [ ] **At most 5 per module, per person, per hour, per server:** 5 `tube add`, 5 `twitch add`,
  5 `cc create`, counted separately. Only real additions count ("already subscribed" doesn't).
  Reply when hit: "You can add 5 YouTube alerts per hour; try again in N minutes."
- [ ] `cc create` is mods only today (exempt), so its limit only matters if creating custom
  commands is opened to everyone.
- [ ] Kept in memory (a sliding one-hour window); a restart resets it, which is fine for spam
  protection.

## Code review (no date yet)

A full read-through of the Rust code, once the feature work settles. Guiding rule: KISS. Prefer
plain functions and data over design patterns, traits and layers of abstraction; remove
abstractions that only have one use.

- [ ] **Rust issues:** trait design and use (object safety, `async_trait` costs, default methods
  hiding missing behaviour, traits with a single implementation), borrowing and ownership
  (needless clones, `Arc`/`Mutex` where a plain value would do, locks held across `.await`,
  `unwrap`/`expect` that can panic in production), error handling (lost context, errors
  swallowed silently), and `unsafe` or blocking calls on the async runtime.
- [ ] **Simplicity:** code that could be shorter or more direct, duplicated logic between the
  `tube`/`twitch`/moderation commands, string building and parsing done by hand in several
  places, modules that grew too large (`transfer.rs`, `moderation.rs`).
- [ ] **Best-practices loop:** run `cargo clippy` with the pedantic group as a review aid (fix
  what reads better, ignore the rest), check dependency freshness and features
  (`cargo tree -d`, unused features), and repeat review → fix → test until a pass finds nothing
  worth changing.
- [ ] Record what was changed and what was deliberately left as is, so the next review starts
  from there.

## Bonus: Stoat

Only after phase 6, and only if the community wants it.

- [ ] Pick a Stoat Rust library, or call its REST/WebSocket API directly (its API is not Discord-compatible).
- [ ] `botsu-stoat` chat adapter; capabilities report what Stoat lacks. Register its invite link (`bot.register_invite`) so `invite` lists it.
- [ ] Music: reuse the LiveKit voice adapter if Stoat bots can join voice (Stoat added LiveKit to self-hosting in February 2026).

---

## Feature × platform matrix

Updated as phases land. ✅ works · 🟡 partial · ❌ not possible · — not started

| Feature | Discord | Fluxer | Stoat (bonus) |
|---|---|---|---|
| Prefix commands | ✅ | ✅ (self-hosted) | — |
| Slash commands | optional | ❌ (not in fluxer-rust 0.2) | — |
| Alerts | ✅ (code, live test pending) | ✅ (code, live test pending) | — |
| Permissions / cleanup / admin | ✅ (code, live test pending) | ✅ (code, live test pending) | — |
| Moderation | ✅ (code, live test pending) | ✅ (code, live test pending) | — |
| Music | 🟡 (worked before the pipe change; retest pending) | ✅ (play, queue, pause tested) | — |

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
