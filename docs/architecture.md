# botsu architecture

Technical overview for contributors and forks. For setup and testing see [testing.md](testing.md);
for what's planned see [roadmap.md](roadmap.md).

## Goals that shape the design

- **One bot, several chat platforms.** Discord and Fluxer today, maybe Stoat later. Features are
  written once and work everywhere.
- **Platform churn stays contained.** Platform libraries are young (fluxer-rust is 0.x); when one
  breaks, the fix should touch one crate.
- **Lean.** One process, no Java, no external database server. SQLite in a single file.

## Crates

```
crates/
├── botsu-core      platform-neutral: adapter traits, commands, alerts, storage
├── botsu-discord   Discord adapter (serenity)       — only crate that imports serenity
├── botsu-fluxer    Fluxer adapter (fluxer-rust)     — only crate that imports fluxer-rust
└── botsu           binary: env config, logging, wiring, one task per platform
```

`botsu-core` must never depend on a platform library. General-purpose crates (HTTP, SQLite, XML)
are fine there, which is why features like alerts live in the core.

## How a command flows

```
 Discord gateway ──► botsu-discord ─┐                         ┌─► ChatAdapter::send_message
                    (translate to   │   Bot::handle_message    │   (back through the same adapter)
 Fluxer gateway  ──► botsu-fluxer  ─┴─► IncomingMessage ──► prefix parse ──► Command::run
                                                                  │
                                                                  └─► Store (SQLite), HTTP (YouTube/Twitch)
```

1. An adapter receives a platform event and translates it into a core `IncomingMessage`
   (platform, guild/server ID, channel ID, author, content). All IDs are strings.
2. `Bot::handle_message` ignores bots, checks the prefix (`BOTSU_PREFIX`, default `$`), looks up
   the command and runs it with a `CommandContext`.
3. The command replies through `ctx.chat`, the same platform's `ChatAdapter`.

Errors: `Error::Usage` and `Error::Unsupported` are shown to the user as-is; anything else is
logged and the user sees a generic message.

### Background work (alerts)

Pollers don't have an incoming message to reply to. Each adapter calls `bot.register_chat(..)`
once connected, and pollers look up `bot.chat(platform)` to post.

## Adapter interface

`ChatAdapter` (in `botsu-core/src/adapter.rs`) is everything the core may ask of a platform:

| Method | Required | Used by |
|---|---|---|
| `platform()`, `capabilities()` | yes | everything |
| `send_message(channel, text)` | yes | replies, alerts |
| `delete_message`, `bulk_delete` | no | cleanup (phase 4) |
| `find_channel(guild, name)` | no | `import` with channel names |

Optional methods have default bodies that return `Error::Unsupported`, so a platform that can't
do something answers "not supported on X" instead of failing oddly. Add methods as features need
them.

### Voice

`VoiceAdapter` (`botsu-core/src/voice.rs`): `user_channel`, `voice_members`, `join`, `play`,
`stop`, `leave`. `play` takes a file path or URL; **ffmpeg** decodes it in both implementations.

| | Discord (`DiscordVoice`) | Fluxer (`FluxerVoice`) |
|---|---|---|
| Transport | songbird 0.6 (Discord voice, DAVE via `davey`) | LiveKit room via fluxer-rust |
| Audio path | ffmpeg → 48 kHz stereo f32 PCM → songbird `RawAdapter` (no decoders compiled in) | fluxer-rust `play_music` (ffmpeg → s16 PCM → LiveKit) |
| Who is in voice | GUILD_CREATE snapshot + VOICE_STATE_UPDATE (no serenity cache) | GUILD_SYNC / PASSIVE_UPDATES snapshots + VOICE_STATE_UPDATE |

`main` creates the voice adapters before the platforms connect, hands them to the `voice` command
(a manual test, bot owners only) and passes each to its platform's `run`, which fills in the
connection and voice states. Phase 6 (music) builds its queue on this interface. The Docker image
carries a static ffmpeg binary (`mwader/static-ffmpeg`) because Debian's package adds ~650 MB of
desktop libraries.

## Data model

One SQLite file: `<BOTSU_DATA_DIR>/botsu.db` (`data/botsu.db` locally, `/data/botsu.db` in Docker).
Code: `botsu-core/src/storage.rs`.

### Migrations

`MIGRATIONS` is an append-only list of SQL scripts. The database's `PRAGMA user_version` records
how many have run; on startup the rest are applied in order, each in a transaction. **Never edit a
migration that has shipped**: add a new one. The database runs in WAL mode.

### `subscriptions`

One row per (source → destination channel). The same source can post to many channels, on any
platform.

| Column | Type | Meaning |
|---|---|---|
| `id` | INTEGER PK | |
| `kind` | TEXT | `youtube` or `twitch` |
| `source_id` | TEXT | YouTube channel ID (`UC…`) or Twitch user ID (numeric, stable across renames) |
| `source_name` | TEXT | Display name when subscribed (for lists) |
| `source_key` | TEXT | Twitch login (for URLs, removal); for YouTube the `@handle` if one was given, else the channel ID |
| `platform` | TEXT | `discord` or `fluxer`: which adapter posts it |
| `guild_id` | TEXT NULL | Server the subscription belongs to (NULL in DMs); scopes `list`/`remove` |
| `channel_id` | TEXT | Destination channel on that platform |
| `created_by` | TEXT | User ID that added it |
| `created_at` | INTEGER | Unix seconds |
| `include_shorts` | INTEGER (0/1) | YouTube: also post Shorts. Default 0 (migration 2) |

Unique on `(kind, source_id, platform, channel_id)`.

`platform` keeps services apart: a Discord subscription is only ever posted through the Discord
adapter, and `list`/`remove` only see the platform they're typed on. Subscriptions aren't tied to
a bot application ID, so swapping the bot account (for example at cutover) keeps them working.
They're also not tied to a Fluxer instance: pointing the bot at a different instance with the same
database would leave Fluxer subscriptions pointing at channels that don't exist there.

### `seen_items`

Everything already handled, so nothing is posted twice, also across restarts. Shared by all
subscriptions of a source.

| Column | Type | Meaning |
|---|---|---|
| `kind` | TEXT | `youtube` or `twitch` |
| `source_id` | TEXT | As in `subscriptions` |
| `item_id` | TEXT | YouTube video ID or Twitch stream ID; `__baseline__` marks "subscribed" |
| `seen_at` | INTEGER | Unix seconds |

Primary key `(kind, source_id, item_id)`. Rows older than a year are pruned.

### `source_state`

Per-source state that isn't an item.

| Column | Type | Meaning |
|---|---|---|
| `kind` | TEXT | Currently only `twitch` |
| `source_id` | TEXT | Twitch user ID |
| `last_live_at` | INTEGER NULL | Last poll that saw the stream live |

Primary key `(kind, source_id)`.

### `pending_items` (migration 2)

YouTube items seen in a feed but not posted yet: scheduled streams and premieres waiting to start,
or items that couldn't be classified because the Data API was unavailable. Checked every poll.

| Column | Type | Meaning |
|---|---|---|
| `kind` | TEXT | `youtube` |
| `source_id` | TEXT | YouTube channel ID |
| `source_name` | TEXT | Channel name for the alert text |
| `item_id` | TEXT | Video ID |
| `title`, `url` | TEXT | For the alert text |
| `added_at` | INTEGER | Unix seconds; dropped after 14 days if it never goes live |

Primary key `(kind, item_id)`.

## Alerts

Code: `botsu-core/src/alerts/`. Both services use their official interfaces only; no web pages
are scraped.

| | YouTube | Twitch |
|---|---|---|
| New items | Official RSS feed `feeds/videos.xml?channel_id=…` (no key, no quota) | Helix `GET /streams` with an app access token |
| Classification | Data API v3 `videos.list` (`snippet.liveBroadcastContent`): `live` / `upcoming` / `none`. 1 quota unit per 50 items, only for new or waiting items | n/a: `/streams` only returns live streams |
| Interval | `YOUTUBE_POLL_SECONDS` (300, min 60), ±10% jitter | `TWITCH_POLL_SECONDS` (120, min 30), ±10% jitter |
| Item | video ID | stream ID (new for every broadcast) |
| Input | `UC…` ID or channel link; `@handle` and `/user/` links via Data API `channels.list` | login or twitch.tv link (resolved to the user ID) |

Each source is fetched once per cycle no matter how many channels subscribe to it. The feed is
used for detection because polling every channel through the Data API would exceed the free
10,000 units/day; the API is only asked about the few items that are new.

**What gets posted for a new YouTube item:**

| Item | With `YOUTUBE_API_KEY` | Without |
|---|---|---|
| Short (feed link `/shorts/<id>`) | "posted a new Short", only to subscriptions with Shorts on | same |
| Regular upload | "posted a new video" | same |
| Live stream | "is live on YouTube" | "posted a new video" |
| Scheduled stream / premiere | waits in `pending_items`; "is live" when it starts | "posted a new video" when it appears |
| Stream that started and ended between polls | "was live on YouTube" | n/a |

**Rules that prevent spam:**

- **On subscribe or import:** everything currently in the feed (or the live stream) is marked
  seen, so only new uploads or streams are posted.
- **YouTube:** videos published more than 7 days ago are never posted, even if unseen (for example
  a video made public long after upload).
- **Twitch:** a new stream starting within 15 minutes of the previous one last being seen live
  counts as a reconnect and isn't announced again.
- **Delivery failures** (missing permissions, deleted channel) are logged and not retried, so one
  bad channel doesn't block others or cause repeats.

**Import** (`alerts/import.rs`) parses free-form pasted lists: `UC…` IDs anywhere, Twitch names in
`a, b and c` lists, `youtube:`/`twitch:` section headings, and channel headings (`(1234…)` IDs,
`#name` or a bare non-Twitch word such as `content-feed`) that decide the destination channel.

## Permissions

Alert commands (`alerts/commands.rs`):

| Subcommand | Allowed |
|---|---|
| `add`, `list`, `remove` | anyone |
| `import`, `tube shorts` | bot owners: `ctx.require_owner()` checks `BOTSU_OWNERS` (IDs from any platform) |

Deliberately lax for now. Refusals of owner-only commands include the author's ID, so setting up
`BOTSU_OWNERS` doesn't need developer mode. Building blocks for finer rules are already there:
subscriptions record `created_by`, and `ChatAdapter::guild_owner` returns a server's owner on both
platforms. Role-based permissions come with phase 4.

## Configuration

Documented in [`.env.example`](../.env.example) and the [README](../README.md#configuration).
Code: `crates/botsu/src/config.rs`. Each setting is looked up in the environment first, then in
the settings file `<BOTSU_DATA_DIR>/botsu.env` (Docker `--env-file` rules: `KEY=value`, `#`
comments, no quotes). Locally the environment comes from `.env` (`scripts/run.ps1`, VS Code launch
configs).

**First start without a token:** the bot writes `botsu.env` from the embedded `.env.example`
(mode 660), logs where it is, and re-reads it every 30 seconds until `DISCORD_TOKEN` or
`FLUXER_TOKEN` is set, then starts. Settings that are present but invalid stop the bot with an
error instead.

**Container start:** `docker/entrypoint.sh` runs as root only to create `/data` and give it (and
files in it) to `PUID:PGID` (default 99:100), then `exec setpriv` drops to that user. Started
with `--user`, it skips that and runs the bot directly.

## Extending

**A new command:** implement `Command` (`name`, `help`, `run`) in the core and register it in
`crates/botsu/src/main.rs` with `bot.register(..)`. Give it its dependencies (store, HTTP client)
through its constructor.

**A new platform:**

1. New crate `botsu-<platform>`; the only one importing that platform's library.
2. Implement `ChatAdapter`; translate incoming messages into `IncomingMessage` and call
   `bot.handle_message`.
3. Call `bot.register_chat(..)` once connected.
4. Add a `Platform` variant and its storage name (`platform_str` / `parse_platform` in
   `storage.rs`), config in `crates/botsu/src/config.rs`, and a task in `main.rs`.

**A new table or column:** append a migration to `MIGRATIONS`.

## Platform library notes

- **fluxer-rust 0.2.1** (pinned with `=`): no slash commands, so commands are prefix-based
  everywhere. botsu discovers the API URL itself from `<instance>/.well-known/fluxer`
  (`endpoints.api_public` + `/v1`), because the library's own lookup uses the wrong path. It pulls
  in LiveKit/libwebrtc unconditionally; their versions are pinned in `Cargo.lock` to the ones
  fluxer-rust was tested with, and Windows builds need `+crt-static` (`.cargo/config.toml`).
- **serenity 0.12**: gateway + REST only, no cache. Needs the privileged Message Content intent
  for prefix commands.
