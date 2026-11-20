# botsu: self-hosted chat bot (Discord + Fluxer)

Replaces a Red-DiscordBot instance ("botsu!", prefix `$`). Background: docs/handover.md.
**Plan and phase checklists: docs/roadmap.md (the source of truth for what's next).**
Technical overview (crates, DB tables, alert rules): docs/architecture.md. Update it when the
schema, adapter trait or alert rules change.

## Decisions (differ from the handover's recommendation)
- **Rust from the start**, not Python first. Accepting churn from young platform libraries.
- **Discord and Fluxer are both required.** Stoat is a bonus only.
- Docker image must work for anyone (not Unraid-specific); native Linux (systemd) is documented too.
- Releases (`v*` tags, .github/workflows/release.yml): Linux .tar.gz + .deb (cargo-deb metadata in
  crates/botsu/Cargo.toml) and Windows .zip. Guides: docs/install-linux.md, docs/install-windows.md.
- Push to `master` publishes the image to Docker Hub (phase 3).

## Layout
- `crates/botsu-core`: platform-neutral core. `ChatAdapter` trait, `IncomingMessage`, command
  routing, and (later) features: alerts, moderation, music queue. **Must not depend on any
  platform crate** (fluxer-rust, serenity, ...).
- `crates/botsu-discord`: the only crate that touches `serenity` (0.12, gateway + REST, no cache).
  Builds a `DiscordChat` per event from `ctx.http`. Needs the privileged Message Content intent.
- `crates/botsu-fluxer`: the only crate that touches `fluxer-rust` (lib name `fluxer`). Translates
  gateway events into core types; implements `ChatAdapter` over Fluxer REST. Fluxer breakage gets
  fixed here.
- `crates/botsu`: the binary. Env config, logging, starts one task per adapter, exits if one dies
  (Docker restarts it).
- `spikes/`: throwaway experiments (phase 0). Not workspace members.

## Phase 2 (alerts) shape
- `botsu-core/src/storage.rs`: SQLite via rusqlite (sync behind a Mutex; queries are tiny).
  Migrations are an append-only list; `PRAGMA user_version` tracks what's applied.
- `botsu-core/src/alerts/`: `youtube.rs` (RSS feed for detection + Data API for handles and live state; no page scraping, by user preference), `twitch.rs` (Helix, app
  token), `mod.rs` (pollers, dedup rules, delivery), `commands.rs` (`tube`, `twitch`).
- Adapters call `bot.register_chat(..)` once connected so pollers can post; `bot.chat(platform)`.
- Permissions (phase 4): bot owner > server owner > admin > mod > everyone; `Command::level()`, checked in
  `Bot::handle_message`; details in docs/architecture.md. Alert commands (user decision, lax): add/list/remove
  anyone; import and tube shorts
  bot owners only (`ctx.require_owner()`, `BOTSU_OWNERS`). Phase 4 adds roles.
- Config: env vars first, then `<data dir>/botsu.env` (auto-created on first start; bot waits for a token).
  Docker entrypoint fixes `/data` ownership and drops to PUID:PGID (99:100). Image: `caaarrrlll/botsu:latest`,
  published by .github/workflows/ci.yml on push to master (secret DOCKERHUB_TOKEN; the Docker Hub
  description is updated by hand, by user choice). Dockerfile uses cargo-chef; PRs build the image without pushing. Don't push images by
  hand any more unless CI is broken. User is fine publishing often.
- Network test: `cargo test -p botsu-core -- --ignored` (hits real YouTube).

## Voice (phase 0 test, phase 6 groundwork)
- `VoiceAdapter` in botsu-core/src/voice.rs; `DiscordVoice` (songbird 0.6, DAVE) and `FluxerVoice` (LiveKit).
  Idle timer: leaves after 10 min without sound (`IDLE_TIMEOUT`, checked every 30 s, one watcher per server).
  `!music test|where` (bot owners) test it on the deployed container. ffmpeg in the image
  is a static binary from mwader/static-ffmpeg (Debian's package made the image 830 MB). Build needs
  libopus-dev (Docker build stage, CI); runtime needs libopus0. Discord plays ffmpeg's raw f32 PCM
  through songbird's `RawAdapter`, which needs symphonia's `pcm` feature (botsu-discord enables it;
  songbird ships symphonia with no codecs, and without it every track errors instantly).

## Moderation (phase 5)
- `botsu-core/src/moderation.rs`: cases (migration 4: `mod_cases`, `scheduled_unbans`, `guild_settings`),
  mod-log via `set modlog`, hierarchy checks in `ModTools::check_target`, temp-ban scheduler.
- Fluxer kick/ban/timeout go through `FluxerChat::call` (fluxer-rust can't send `X-Audit-Log-Reason`).

## Music (phase 6)
- `botsu-core/src/music.rs` (queue, player task per server, `!music` group) and `ytdlp.rs` (botsu's own
  yt-dlp in `<data dir>/bin`, updated daily; `BOTSU_YTDLP` overrides). Image ships Deno for YouTube.
- Commands are grouped: `!mod …`, `!admin …`, `!music …` (`command::Group`).

## Current phase
Running on Unraid from the Docker Hub image (CI publishes on push; docs-only pushes skip the image).
Unraid template on the server matches deploy/unraid/my-botsu.xml (migrated 2026-11-20).
Live-tested: alerts (testing.md 5: uploads, YouTube live, Twitch live, no reposts, on both platforms),
`cleanup messages`, Fluxer music playback, phase 0 voice. Still to test: server tools (6), moderation
(7, incl. mute/moddm), Discord music and volume (8), alert embeds and mentions, the Fluxer API spike.
Then phase 7 (cutover, docs/cutover.md). Red still runs on the Discord servers with `$`, so botsu
runs with `BOTSU_PREFIX=!` (code default stays `$`); Red is not to be changed until phase 7.
Settings export/import is built (`transfer.rs`, `admin export|import`; docs/settings-migration.md;
`transfer::SETTINGS` must list every `guild_settings` key, a test enforces it). Parked: Twitch EventSub, YouTube
cookies, Stoat. Branch protection waits for the user's git flow. Keep docs/roadmap.md checkboxes
up to date as work lands.

## Alerts (beyond phase 2)
- Per subscription: custom text (`message`, placeholders `{name} {title} {url} {game} {event}`) and a
  mention (role, `user:<ID>`, everyone/here); migration 5. Names/titles are Markdown-escaped (`escape_md`).
- Delivery (`alerts::deliver`): botsu's own embed (`ChatAdapter::send_embed`, large image: YouTube
  maxres/hq thumbnail, Twitch live frame 1280x720 with a cache-buster) unless `set embeds off`
  (`guild_settings.alert_embeds`); falls back to plain text. Text above an embed never has a bare link.
- Adding a source that's already followed doesn't touch `seen_items` (the poller owns it).

## fluxer-rust 0.2.1 notes (pinned with `=`; vendored with patches in vendor/fluxer-rust, see vendor/README.md)
- Patches: `Guild.voice_states` (GUILD_CREATE), `FluxerVoiceConnection::play_input` (ffmpeg with reconnect
  flags, stdin input, errors returned, pause flag, volume). Before bumping, port these or drop them if upstream has them.
- No interactions/slash commands: prefix commands only on Fluxer.
- API URL comes from discovery: `<instance>/.well-known/fluxer` (domain root) → `endpoints.api_public`
  + `/v1`. botsu fetches it itself (`discover` in botsu-fluxer) because fluxer-rust's
  `get_well_known_fluxer` looks under `/api/` and its typed struct is brittle. fluxer-rust's
  built-in default `api.fluxer.app/v1` is outdated (now `fluxer.com/api/v1`); don't rely on it.
- Bot invites: OAuth2 `<instance>/oauth2/authorize?client_id=..&scope=bot&permissions=..`
  (docs.fluxer.com/http-api/oauth2). Fluxer docs moved from docs.fluxer.app to docs.fluxer.com.
- `Client::start` reconnects by itself; it logs with `eprintln!`, not `tracing`.
- Voice: `ctx.join_voice` gives a LiveKit `FluxerVoiceConnection`. Its `play_music` hardcodes the
  ffmpeg args (no `-reconnect` flags) and the `audio_source` is private, so the phase 6 music
  pipeline will likely need its own LiveKit publishing (use `conn.room`) or a fork.
- It depends on `livekit` (libwebrtc) unconditionally: slow first build; Linux build needs
  `pkg-config libglib2.0-dev libva-dev`.

## Conventions
- Secrets only in env vars (env file on the server, `--env-file`); never commit tokens. `.env.example` lists them.
- Keep `.env.example` in sync with every key in the user's `.env` (and any new env var the code reads),
  with a comment saying what it's for. Never read `.env` values; compare key names only.
- Run locally: VS Code launch configs `botsu (debug)` / `botsu (release)` (CodeLLDB) or `scripts/run.ps1 [-Release]`.
- Deploys as a Docker container on Unraid (LF line endings, enforced by `.gitattributes`).
- yt-dlp must be updatable without an image rebuild; resolve stream URLs at play time; run it as a subprocess.
- Adapter trait methods are opt-in: default bodies return `Error::Unsupported`.
- `cargo fmt`, `cargo clippy --all-targets`, `cargo test` before committing.
