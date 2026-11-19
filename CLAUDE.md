# botsu: self-hosted chat bot (Discord + Fluxer)

Replaces a Red-DiscordBot instance ("botsu!", prefix `$`). Background: docs/handover.md.
**Plan and phase checklists: docs/roadmap.md (the source of truth for what's next).**

## Decisions (differ from the handover's recommendation)
- **Rust from the start**, not Python first. Accepting churn from young platform libraries.
- **Discord and Fluxer are both required.** Stoat is a bonus only.
- Docker image must work for anyone (not Unraid-specific); native Linux (systemd) is documented too.
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

## Current phase
Phase 1: code done for both adapters; live testing on the user's self-hosted Fluxer (Unraid) and
several Discord servers, following docs/testing.md. Red still runs on those Discord servers with
`$`, so botsu runs with `BOTSU_PREFIX=!` (code default stays `$`). Red is not to be changed until
botsu is definitely replacing it (phase 7). Phase 0 spikes are still open. Keep the checkboxes in
docs/roadmap.md up to date as work lands.

## fluxer-rust 0.2.1 notes (pinned with `=`; read its source in ~/.cargo/registry before bumping)
- No interactions/slash commands: prefix commands only on Fluxer.
- `Client::start` reconnects by itself; it logs with `eprintln!`, not `tracing`.
- Voice: `ctx.join_voice` gives a LiveKit `FluxerVoiceConnection`. Its `play_music` hardcodes the
  ffmpeg args (no `-reconnect` flags) and the `audio_source` is private, so the phase 6 music
  pipeline will likely need its own LiveKit publishing (use `conn.room`) or a fork.
- It depends on `livekit` (libwebrtc) unconditionally: slow first build; Linux build needs
  `pkg-config libglib2.0-dev libva-dev`.

## Conventions
- Secrets only in env vars (env file on the server, `--env-file`); never commit tokens. `.env.example` lists them.
- Deploys as a Docker container on Unraid (LF line endings, enforced by `.gitattributes`).
- yt-dlp must be updatable without an image rebuild; resolve stream URLs at play time; run it as a subprocess.
- Adapter trait methods are opt-in: default bodies return `Error::Unsupported`.
- `cargo fmt`, `cargo clippy --all-targets`, `cargo test` before committing.
