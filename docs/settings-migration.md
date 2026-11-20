# Plan: export and import a server's settings

Status: built (2026-11-20) in `botsu-core/src/transfer.rs`, steps 1–5 below; live test pending
(testing.md section 9). Not built yet: `--create-channels` and `import from <platform> <server>` (step 7).
Tracked in [roadmap.md](roadmap.md).

## Goal

Move one server's botsu setup to another server. The two servers can be on different
platforms (Discord ↔ Fluxer), on different Fluxer instances, or under a different botsu
install. The file is platform-neutral, so the same code works in all four directions:

| From → to | Example |
|---|---|
| Discord → Fluxer | the community moves to Fluxer |
| Fluxer → Discord | the other way |
| Discord → Discord | a server is rebuilt or replaced |
| Fluxer → Fluxer | moving to a self-hosted (or another) instance; also covers the roadmap item "record the Fluxer instance on subscriptions" |

Only server configuration moves, never data about users.

## What's in the file, and what isn't

| Data | Table | Exported | Why |
|---|---|---|---|
| YouTube and Twitch subscriptions: source ID, name, key, target channel, Shorts on/off | `subscriptions` | yes | the main reason for this feature |
| Admin and mod roles | `guild_roles` | yes | permissions setup |
| Mod-log channel | `guild_settings` `modlog_channel` | yes | |
| Music volume | `guild_settings` `music_volume` | yes | |
| Mute role, mod DMs on/off | `guild_settings` `mute_role`, `mod_dm` | yes | mute role is mapped like the admin/mod roles |
| Custom alert text and mention | `subscriptions.message`, `.mention` | yes | the mention role is mapped like the others |
| Scheduled unmutes | `scheduled_unmutes` | **no** | about users |
| Who added a subscription | `subscriptions.created_by` | **no** | user ID; the importer becomes the creator |
| Cases, warnings | `mod_cases` | **no** | about users; IDs don't carry across platforms anyway |
| Pending temp-bans | `scheduled_unbans` | **no** | about users |
| Seen and pending videos, live state | `seen_items`, `pending_items`, `source_state` | **no** | tracked per source, not per server; the target bot keeps its own |

`guild_settings` keys are only exported if they're registered (see "Settings registry" below). A
key added later stays out of exports until someone decides how it carries over. That way nothing
user-related leaks in by accident.

## The file

JSON, versioned, readable by people. Channels and roles are listed once, with their names, and
everything else points at them by their old ID. Mapping old IDs to new ones then happens in one
place.

```json
{
  "botsu_export": 1,
  "exported_at": "2026-11-20T12:00:00Z",
  "botsu_version": "0.1.0",
  "from": { "platform": "discord", "guild_id": "1234", "guild_name": "Example" },
  "channels": {
    "111": { "name": "youtube-alerts" },
    "222": { "name": "mod-log" }
  },
  "roles": {
    "333": { "name": "Moderators" },
    "444": { "name": "Admins" }
  },
  "permissions": { "admin_roles": ["444"], "mod_roles": ["333"] },
  "settings": { "modlog_channel": { "channel": "222" }, "music_volume": 80 },
  "subscriptions": [
    { "kind": "youtube", "source_id": "UC...", "source_name": "Some Channel",
      "source_key": "UC...", "channel": "111", "include_shorts": false },
    { "kind": "twitch", "source_id": "12345", "source_name": "somestreamer",
      "source_key": "somestreamer", "channel": "111" }
  ]
}
```

- **Source IDs don't depend on the platform.** YouTube channel IDs and Twitch user IDs mean the
  same thing on every server, so they're copied as they are. Twitch logins can change, so
  `source_key` is refreshed from Helix on import when Twitch is configured.
- **Unknown fields are ignored on import.** A newer `botsu_export` version is refused, with a
  message saying which version this botsu reads.
- **No secrets or tokens are in the file.** Only names and IDs that any member of the server can
  see.

## Mapping onto the target server

IDs never match between servers, so the importer works out where everything goes:

1. **Channels by name.** Matching ignores case and treats a leading `#`, spaces, `-` and `_` as
   the same. Discord names are lowercase with dashes, and Fluxer allows more, so "YouTube Alerts"
   matches `youtube-alerts`. Only text channels the bot can post in count.
2. **Roles by name.** Matching ignores case and must be exact otherwise; a near miss is not
   enough for permissions. The `@everyone` role is never mapped.
3. **Manual overrides on the command message**, one per line: `map <old name or ID> = <new>`,
   where the new side is a mention, an ID or a name (`map mod-log = #moderation`,
   `map Mods = @Moderators`). For anything that didn't match or matched the wrong thing.
4. **Missing channels.** Anything still unmatched is listed in the preview and skipped. With
   `--create-channels`, botsu creates the missing text channels first (bot needs Manage
   Channels). That's off by default because it's the only step that changes the server itself.
5. **Missing roles.** Roles are never created: a role created by the import would have no members
   and no permissions anyone chose.

## Commands

Under `!admin`, next to the other server tools:

| Command | Who | Does |
|---|---|---|
| `admin export` | admins | posts the file as an attachment (`botsu-<platform>-<server>-<date>.json`) |
| `admin import` + attached file | server owner (or bot owner) | reads the file, maps it, posts a **preview**; nothing changes yet |
| `admin import confirm` | the same person, within 10 minutes | applies the preview |
| `admin import cancel` | the same person | drops it |

Options on `admin import`: `--replace` and `--create-channels`, plus `map` lines (see above).

- **Why the server owner.** An import can change who counts as admin (admin roles). That is the
  same level `set adminrole` needs today.
- **Merge or replace.** The default merges: it adds what's missing, keeps what's there, and lists
  duplicates as "already there". `--replace` first removes this server's subscriptions, admin and
  mod roles and exported settings, then imports. The preview says so in bold.
- **The preview** lists every item with its result: "added", "already there", "channel not
  found", "Twitch isn't set up on this bot", and so on. It's posted as a file if it gets too long
  for one message (Discord allows 2000 characters).
- **The pending import is held in memory,** keyed by (platform, server, user). A restart drops
  it; run the import again.

## How applying works

- **Subscriptions go through the same code path as `tube add` / `twitch add`.** The baseline
  marker is recorded the same way, so the target posts only uploads and streams that happen
  after the import, never the source's backlog. Each target channel gets one notice ("YouTube
  alerts from this server's import will post here"), like `tube import` posts today. That also
  proves the bot can post there; channels where it can't are reported, not saved.
- **YouTube without an API key, or Twitch without credentials, on the target bot.** The
  subscriptions are still saved and the preview warns about them. Twitch ones won't post until
  `TWITCH_CLIENT_ID` and `TWITCH_CLIENT_SECRET` are set.
- **Roles and settings are written in one SQLite transaction.** A failure part way leaves the
  server as it was. Subscriptions are applied one by one, because each one checks the channel;
  the result lists any that failed.
- **Same source in the same channel twice** (both bots run during a migration): the existing
  `UNIQUE (kind, source_id, platform, channel_id)` constraint makes the second one "already
  there".

## Same botsu, two servers

A single botsu serving both Discord and Fluxer (the current setup) can copy a server's settings
directly, without the file: `admin import from <platform> <server id>`. This builds the same
in-memory export from the database and runs the same preview and confirm steps. It needs the
importer to be a bot owner, because it reads another server's settings. It's a small extra on
top of the file path, so it comes last.

## Transport changes needed

| Piece | Discord (serenity) | Fluxer (fluxer-rust) |
|---|---|---|
| Send a file: new `ChatAdapter::send_file(channel, name, bytes, text)` | `CreateMessage::add_file(CreateAttachment::bytes(..))` | `Http::send_files` / `send_message_with_files` (multipart, present in 0.2.1) |
| Read attachments: new `IncomingMessage.attachments: Vec<Attachment { url, filename, size }>` | `Message.attachments` | `Message.attachments` (in the model) |
| Download | reqwest on the attachment URL | same; self-hosted instances may need the bot token on the media URL, so check on the instance |
| Create a channel (only for `--create-channels`) | `GuildId::create_channel` | `POST /guilds/{id}/channels` (Manage Channels) |
| List channels with names and types | `GuildId::channels` | `get_guild_channels` (already used by `find_channel`) |

Both new adapter methods are opt-in, as the conventions require. A platform without them
answers "exports need file uploads, which this platform doesn't support yet".

Limits: imports are capped at 1 MB and 2000 subscriptions, and the file must parse as
`botsu_export` JSON before anything else happens.

## Settings registry

A small table in the core that says how each `guild_settings` key travels:

```rust
enum Portable { Channel, Role, Value, Never }
const SETTINGS: &[(&str, Portable)] = &[
    (MODLOG_KEY, Portable::Channel),
    (music::VOLUME_KEY, Portable::Value),
    (MUTE_ROLE_KEY, Portable::Role),
    (MOD_DM_KEY, Portable::Value),
    (EMBEDS_KEY, Portable::Value),
];
```

Export skips keys that aren't listed. A unit test fails if a key used with `set_guild_setting`
is missing from the registry, so each new setting has to be classified when it's added.

## Order of work

1. File format and its types (`serde`) with round-trip tests: export → JSON → import plan,
   using the in-memory `Store`. No platform code needed.
2. The mapper: channel and role name matching, overrides, preview text. Pure functions,
   unit-tested with made-up channel and role lists. Cover the Discord → Fluxer name differences.
3. Settings registry and its test.
4. Adapter pieces: `send_file`, `attachments`, channel list with types (both platforms).
5. `admin export`, then `admin import` / `confirm` / `cancel`, with merge and `--replace`.
6. Live test (a new section in testing.md): export from a Discord test server, import into a
   Fluxer test server, check channels, roles, mod log, volume and the subscriptions. Then
   confirm nothing is reposted, and repeat in the other direction.
7. Optional: `--create-channels`, then `import from <platform> <server>` for a single botsu.

## Open questions

- Should `export` include the YouTube/Twitch names only, for a file people can read and edit
  by hand? (The current plan keeps IDs too, since names aren't unique.)
- Should an import into a server that already has subscriptions warn when the same source
  would end up posting to two channels? It's allowed today.
- Custom alert text and mentions now exist (migration 5): `message` copies as is, `mention`
  goes through the role mapping (`everyone`/`here` copy as is).
