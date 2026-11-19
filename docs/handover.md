# Handover: custom chat bot (Red-DiscordBot replacement)

**Date:** 2026-11-18
**Purpose:** starting point for a new coding project. It collects everything decided and learned so far, so the project can begin without the original discussion. Copy this file into the new project, for example as `docs/handover.md`, and use the seed `CLAUDE.md` at the end.

---

## 1. Why this project exists

There's a Red-DiscordBot instance ("botsu!", command prefix `$`) running on an Unraid home server. Two motivations:

1. **Memory:** almost all of Red's memory is **Lavalink**, the Java audio server used by Red's built-in Audio cog. Red itself is light.
2. **Platform flexibility:** the owner is considering a self-hosted Discord alternative, **Fluxer** or **Stoat** (formerly Revolt). Red is tied to Discord and can't follow. A custom bot with a platform-neutral core could run on Discord *and* the new platform, so the community can move gradually.

**Not a goal:** cloning all of Red. Only the features actually used (section 3).

---

## 2. Current setup (what's being replaced)

| | |
|---|---|
| Host | Unraid 7.3.4, Docker 29.8.2, about 62 GB RAM. Every service runs as a Docker container. |
| Image | `phasecorex/red-discordbot:full`, with appdata in `/mnt/user/appdata/redbot` |
| Audio | Red's built-in Audio cog: managed Lavalink **3.7.13+red.5** plus `youtube-plugin` 1.18.0. Red only accepts Lavalink **v3**. |
| Heap | Was **31G** (Red chooses it from host RAM). Set to **1G** on 2026-11-18 with `$llset heapsize 1G`. Red shows a "you should not be running this command" prompt with a confirmation token; that's normal. |
| YouTube fix | 2026-11-18: the plugin jar was replaced with snapshot build `f45bbb7` under the old filename. Red only checks the filename; managed mode rewrites `application.yml` on every start. See `redbot-youtube-fix.md` in the server notes. |

**Memory, measured 2026-11-18** (`docker top red-discordbot -o pid,rss,comm`):

| Process | Before (heap 31G) | After (heap 1G) |
|---|---|---|
| `java` (Lavalink) | about 890 MB | about 435 MB, and grows towards about 1–1.2 GB |
| `python` (Red with all cogs) | about 82 MB | about 83 MB |
| Container total (`docker stats`) | about 1 GB | 614 MB (includes file cache) |

**Takeaway:** the bot logic costs about 80 MB; the audio stack costs 400 MB–1.1 GB. **Replacing the audio pipeline is the efficiency win**, not rewriting the rest.

---

## 3. Features to cover (the Red cogs actually used)

Loaded and wanted: **admin, audio, cleanup, mod, permissions, streams, Tube**, plus downloader, which is Red-internal and has no equivalent needed. Unloaded as unused on 2026-11-18: aimage, crier, customcom, image, trivia.

| Red cog | What the new bot needs | Effort | Notes |
|---|---|---|---|
| **permissions** | Who may run which command | Almost free | On Discord, use the built-in slash-command permissions (Server Settings → Integrations). On other platforms, use simple role checks in the core. |
| **cleanup** | Bulk-delete messages by count, user or bots | Small | Discord bulk-deletes only messages younger than 14 days, at most 100 per call. |
| **admin** | Add/remove roles, announcements | Small | |
| **Tube** | Post when a YouTube channel uploads | Small–medium | Poll `https://www.youtube.com/feeds/videos.xml?channel_id=<id>`; no API key. Keep a "seen video IDs" table to avoid double posts. |
| **streams** | Post when a Twitch/YouTube stream goes live | Medium | Twitch Helix API (app access token, polling or EventSub). YouTube live detection has no clean free API: RSS heuristics or the Data API (limited quota). |
| **mod** | Kick, ban, temp-ban, timeout, warnings, mod-log cases | Medium | Needs a database and a scheduler (temp-ban expiry). Discord has native timeouts; no mute role needed. |
| **audio** | Play, skip, stop, queue, now-playing; YouTube links and search | **Large** | The biggest piece. Design in section 5. |

Plus the "framework" Red gave for free: settings storage, help text, error handling, logging, deployment.

---

## 4. Target platforms

| | Discord | **Fluxer** | **Stoat** (formerly Revolt) |
|---|---|---|---|
| Bot API | Discord | **Designed to match Discord's** (REST + gateway). Bots set a custom API/gateway URL for self-hosted instances. | **Its own API**, not Discord-compatible |
| Voice | Discord voice (Opus over UDP, plus DAVE end-to-end encryption) | **LiveKit** | **LiveKit** (added to the self-host stack in February 2026; "extra configuration needed") |
| Libraries seen | serenity/poise, songbird (Rust); discord.py (Python) | `fluxer-rust` 0.2.x (young; has a voice join/play example) | community libraries for Python, Rust, JS |
| Self-host | n/a | Docker Compose "monolith" | Docker Compose, several services |

**Key consequence:** neither alternative uses Discord's voice protocol. Red Audio, Lavalink and discord.py voice are all useless there. **Music on Fluxer/Stoat means joining a LiveKit room and publishing an audio track.** LiveKit has official SDKs for Python, Rust, Go and Node.

**Open questions, to verify first (phase 0):**
1. **Can bot accounts join voice** (get LiveKit room access) on Fluxer and on Stoat? This decides whether music is possible there at all.
2. **How stable are the APIs?** Stoat is mid-rebrand and reorganising; Fluxer's libraries are 0.x. Expect breaking changes.
3. **Hosting cost:** the platform stack (database, storage, LiveKit) will probably use more RAM than the bot saves. LiveKit needs **UDP ports** forwarded on the router.
4. Does Fluxer's "Discord-compatible" API cover **slash commands / interactions**, or only messages and the gateway? That affects whether prefix commands are needed.

---

## 5. Architecture

**Platform-neutral core with adapters:**

```
            ┌─────────────────────────── core ───────────────────────────┐
            │  commands · music queue · Tube/stream pollers · moderation  │
            │  logic · scheduler (temp-bans) · storage (SQLite/Postgres)  │
            └───────┬──────────────────────┬──────────────────────┬───────┘
  chat adapter:     Discord                Fluxer                 Stoat
  voice adapter:    Discord voice          LiveKit ◄──────────────┘
                    (songbird / discord.py)   (one adapter serves both)
```

- **Chat adapter interface (sketch):** connect; receive commands (prefix and/or slash); send, edit and delete messages; bulk-delete; role operations; kick, ban and timeout; list members and channels. Each platform implements what it supports and reports what it doesn't.
- **Voice adapter interface (sketch):** `join(channel)`, `play(pcm/opus stream)`, `pause`, `resume`, `stop`, `leave`, and events (`track_end`, `disconnected`).
- **Alerts** (Tube/streams) are pure "post a message to channel X", which makes them the easiest to make cross-platform. **Build them first.**

### Music pipeline (no Java)

```
search/URL ──► yt-dlp (Python library) ──► stream URL ──► ffmpeg ──► Opus/PCM ──► voice adapter
```

Lessons and gotchas, collected from the discussion:
- **Resolve the stream URL when a track *starts*, not when it's queued.** YouTube URLs expire after a few hours.
- **Run yt-dlp in a background thread/executor.** A direct call blocks the event loop for seconds.
- **ffmpeg reconnect flags:** `-reconnect 1 -reconnect_streamed 1 -reconnect_delay_max 5`.
- **Keep yt-dlp updatable** without rebuilding the image (`pip install -U yt-dlp` at start, or a command). YouTube breaks extractors regularly; yt-dlp usually fixes it within days. That's easier than the Lavalink plugin-jar swap needed on 2026-11-18.
- yt-dlp costs about 50–100 MB while extracting; ffmpeg about 20–40 MB per active stream.
- Expect YouTube anti-bot measures (cookies, tokens) at some point. They affect every bot regardless of language.
- **Discord voice:** check that the chosen library supports Discord's current voice encryption (DAVE end-to-end encryption) before relying on it. A 10-line prototype that joins and plays a file answers this.

---

## 6. Language and library choice

| | Python | Rust |
|---|---|---|
| Discord | discord.py (voice built in, needs PyNaCl) | serenity + poise (commands), **songbird** (voice, no Lavalink) |
| Fluxer | Discord-style libraries may partly work (unverified) | `fluxer-rust` (0.2.x) |
| Stoat | community library | community library |
| LiveKit | official Python SDK | official Rust SDK |
| yt-dlp | import directly | run it as a subprocess |
| Estimated RAM | about 110–190 MB including yt-dlp/ffmpeg | **about 30–70 MB** |
| Build speed | fastest | slower; young platform libraries may churn |

**Recommendation from the discussion:** **Python first** to prove the platform questions (section 4) and get alerts working quickly. Port the core to Rust later *if* memory matters. On a 62 GB host, the saving is about 0.5–1 GB, so it's a learning project, not an operational need.

**C++:** D++ (DPP) supports voice, but it was ruled out because Rust gives similar efficiency with memory safety and a better bot ecosystem.

---

## 7. Alternative path: a Red cog without Lavalink (if staying on Discord)

If Fluxer/Stoat doesn't happen, the cheapest big win is **a custom music cog for Red** instead of a new bot:
- **Red doesn't need Java;** only the built-in Audio cog does. `$unload audio` stops Lavalink, and the bot drops to about 80 MB.
- The custom cog uses discord.py voice + yt-dlp + ffmpeg. Declare `yt-dlp` and `PyNaCl` in the cog's `info.json` `requirements`, and Red's Downloader installs them.
- Command names can't clash with the built-in Audio cog (`play` and so on), so unload Audio first.
- Check that ffmpeg is in the image: `docker exec red-discordbot ffmpeg -version`.
- Estimated total: about 130–190 MB, against 520 MB–1.2 GB now. About 200–400 lines for play/skip/stop/queue/now-playing.
- Lighter Lavalink replacements aren't an option: Red only talks to Lavalink **v3**, and the alternatives target v4.

The music core from section 5 can be written so it's reusable both inside a Red cog and inside the new bot.

---

## 8. Suggested phases

| Phase | Goal | Done when |
|---|---|---|
| **0. Spikes** (throwaway) | Answer the open questions in section 4 | A test Fluxer (and/or Stoat) instance runs. A bot account logs in and posts. A bot joins a voice room and plays a local file through LiveKit. A Discord bot plays a file through the chosen library (DAVE OK). |
| **1. Skeleton** | Core + one chat adapter + config + logging + Docker image | The bot connects and answers `ping` on Discord **and** one alternative platform. |
| **2. Alerts** | Tube (YouTube RSS) + streams (Twitch) | New uploads and live streams are posted on both platforms without duplicates. |
| **3. Moderation** | cleanup, admin, mod (cases, temp-ban scheduler), permissions | Parity with the Red features in use. |
| **4. Music** | yt-dlp/ffmpeg pipeline + Discord voice + LiveKit voice | Play, queue and skip work on both. Survives a network hiccup. |
| **5. Cutover** | Replace Red | Run side by side with Red for a while, then retire Red. |

---

## 9. Deployment conventions (match the existing server)

- **Docker container on Unraid, with an Unraid template.** Keep a template snapshot in the server notes (`unraid-templates/`).
- **Secrets:** never in the template or repo. Use the existing pattern: a root-only env file in appdata (for example `/mnt/user/appdata/secrets/bot.env`, chmod 600), loaded with `--env-file` in Extra Parameters.
  - **No quotes** in env files: Docker keeps them as part of the value.
  - After editing the env file, **recreate** the container (Edit → Apply); a restart doesn't re-read it.
- **Unraid gotcha learned during the Caddy migration:** if a container's *first* start fails (for example on a port clash), Docker 29 can leave it with **no network** (`docker inspect` shows `"Networks": {}`). Fix: `docker rm -f` it and create it again. A bot with no published ports is unlikely to hit this, but LiveKit-related ports might.
- **Database:** SQLite to start. Later possibly the planned **shared PostgreSQL** on the server (it's the Immich Postgres image, which is standard PostgreSQL 18 with vector extensions; one database and login per app; see the server roadmap section 2).
- **Reverse proxy:** Caddy (`caddy/README.md` in the server notes) if the bot or platform needs a public HTTPS endpoint, for example Twitch EventSub callbacks or a Fluxer/Stoat web UI. Cloudflare-proxied names have a **100 MB upload limit**.
- **Line endings:** LF for anything that runs on Unraid.

---

## 10. References

- Fluxer: [roadmap 2026](https://fluxer.app/blog/roadmap-2026), [how it was built](https://fluxer.app/blog/how-i-built-fluxer-a-discord-like-chat-app), [fluxer-rust](https://docs.rs/crate/fluxer-rust/0.2.1/source/README.md), [Discord-compatible interface notes](https://synthetic-heart.readthedocs.io/en/latest/_sources/fluxer_interface.rst.txt)
- Stoat: [developer docs](https://developers.stoat.chat/print.html), [GitHub topic](https://github.com/topics/stoat-chat), [self-host voice (LiveKit) commit](https://git.morozoff.pro/aiden/revolt-docker/commit/54a88cf6cb238d4e96e323b045b78b776616c069)
- LiveKit: official SDKs for Python, Rust, Go and Node (docs.livekit.io)
- Discord (Rust): serenity, poise, songbird. Discord (Python): discord.py.
- yt-dlp: github.com/yt-dlp/yt-dlp
- Red cog index (to check for existing non-Lavalink music cogs): index.discord.red
- Server notes: `redbot-youtube-fix.md` (the Lavalink plugin swap, memory figures, heap setting)

---

## 11. Seed `CLAUDE.md` for the new project

```markdown
# <project name>: self-hosted chat bot (Discord + Fluxer/Stoat)

Replaces a Red-DiscordBot instance. Full background: docs/handover.md (read it first).

## Goals
- Features: Tube (YouTube upload alerts), streams (Twitch/YouTube live alerts), cleanup, admin,
  mod (cases, temp-bans), permissions, music (yt-dlp + ffmpeg, no Java/Lavalink).
- Platform-neutral core + chat adapters (Discord, Fluxer, Stoat) + voice adapters
  (Discord voice, LiveKit). Build alerts first, music last.
- Memory target: well under Red's 520 MB–1.2 GB. Python first (about 110–190 MB), Rust later (about 30–70 MB).

## Current phase
Phase 0: spikes. Verify that bots can join LiveKit voice on Fluxer/Stoat, DAVE support for Discord voice,
and Fluxer slash-command support. Spikes go in spikes/ and are throwaway.

## Conventions
- Secrets only in env vars (env file on the server, --env-file); never commit tokens. Ship a .env.example.
- Deploys as a Docker container on Unraid (LF line endings).
- yt-dlp must be updatable without an image rebuild.
- Resolve stream URLs at play time, and run yt-dlp off the event loop.
```
