# Testing botsu live

How to get tokens and run botsu against real Discord servers and a self-hosted Fluxer instance.
Until phase 3 publishes an image to Docker Hub, the quickest way is to run from source on your PC.

## 1. Discord bot

1. Go to <https://discord.com/developers/applications> → **New Application** (for example "botsu-dev").
2. **Bot** page:
   - **Reset Token** and copy it → `DISCORD_TOKEN`.
   - Under **Privileged Gateway Intents**, turn on **Message Content Intent**. Without it the bot
     sees messages but their text is empty, so prefix commands do nothing.
   - Optionally turn off **Public Bot** so only you can invite it.
3. Invite it to each test server (you need Manage Server there). Replace `APP_ID` with the
   **Application ID** from the **General Information** page:

   ```
   https://discord.com/oauth2/authorize?client_id=APP_ID&scope=bot&permissions=84992
   ```

   `84992` = View Channels + Send Messages + Embed Links (for alert link previews) + Read Message
   History. Once the bot is connected, its log prints this link with the right ID filled in.
   Later phases need more (Manage Messages, Manage Roles, Kick, Ban, Moderate Members, Connect,
   Speak); re-invite with a new link then.

**Red runs on the same servers with `$`.** Use a different prefix while testing, for example
`BOTSU_PREFIX=!` (already set in `.env.example`), otherwise both bots answer `$ping` and `$help`.
Switch back to `$` only at cutover (roadmap phase 7), once botsu is definitely replacing Red.

## 2. Fluxer bot on the self-hosted instance

1. Set `FLUXER_URL` to your instance's web address, the one you open in the browser (for example
   `https://fluxer.example.com`). That's all: the bot reads `<address>/.well-known/fluxer` at
   startup and finds the API URL itself. To check it by hand, open that address in a browser;
   it should show JSON with `endpoints.api_public` and `features.voice_enabled` (phase 0 needs
   `true` for music on Fluxer).
2. In **your instance's** web app (not fluxer.app; accounts are per instance), create the bot:
   - If there's no developer section in Settings, turn on **Developer Mode** first. Then go to
     **Settings → Developer** (some versions call it **Applications**).
   - **Create application**. Each application comes with exactly one bot account.
   - Generate the **bot token** and copy it straight away; it may only be shown once.
     It looks like `<application id>.<secret>`. That whole string → `FLUXER_TOKEN`.
     A lost or leaked token can be reset from the same page.
3. Add the bot to a test community. Fluxer uses an OAuth2 link like Discord's, but the web app may
   not show a ready-made one; build it yourself. `APP_ID` is the application ID (also the part of
   the token before the `.`):

   ```
   https://fluxer.example.com/oauth2/authorize?client_id=APP_ID&scope=bot&permissions=84992
   ```

   Open it while logged in, pick the community and approve. You need Manage Community (Manage
   Guild) or Administrator there. With non-zero `permissions`, Fluxer creates a role named after
   the bot. `84992` is View Channels + Send Messages + Embed Links + Read Message History *if* Fluxer uses
   Discord's permission numbers (it aims to be compatible); if the role looks wrong, use
   `permissions=0` and allow the bot in the channel's permission settings instead.
   If the bot is private, only its owner can add it.

The OAuth2 route and parameters come from Fluxer's API docs
(<https://docs.fluxer.com/http-api/oauth2/>); the web-app page path and the menu names come from
community guides, not a tested walkthrough. If your instance differs, correct this section.

## 3. Run from source

First `cp .env.example .env` and fill in `DISCORD_TOKEN`, `FLUXER_TOKEN` and `FLUXER_URL`.
Every option below reads `.env`; stop the bot with Ctrl+C (or the red square in VS Code).

**VS Code** (needs the recommended extensions rust-analyzer and CodeLLDB):

| What | How | Mode |
|---|---|---|
| Debug (breakpoints) | Run and Debug panel → **botsu (debug)** → F5 | debug build, `RUST_LOG` from `.env` |
| Production | Run and Debug → **botsu (release)** → Ctrl+F5 | release build, `RUST_LOG=info` |
| Run without the debugger | Terminal → Run Task → **botsu: run (debug)** / **botsu: run (release)** | as above |
| Checks before committing | Run Task → **check: all (fmt, clippy, test)** | |

Ctrl+Shift+B builds the debug binary. The first release build takes a few minutes (LTO).

**Terminal (PowerShell):**

```powershell
./scripts/run.ps1            # debug
./scripts/run.ps1 -Release   # production
```

**Git Bash / Linux:**

```sh
set -a; . ./.env; set +a
cargo run -p botsu           # add --release for production
```

Expected log lines:

```
INFO botsu: starting botsu 0.1.0 prefix="!"
INFO botsu_fluxer: Fluxer instance discovered instance="https://fluxer.example.com" ... voice_enabled=true self_hosted=true
INFO botsu_fluxer: using Fluxer API api_url=https://fluxer.example.com/api/v1
INFO botsu_fluxer: Fluxer token OK user=botsu-dev
INFO botsu_fluxer: connected to Fluxer user=botsu-dev
INFO botsu_discord: connected to Discord user=botsu-dev guilds=3
```

## 4. What to test (phase 1 checklist)

| Test | Discord | Fluxer |
|---|---|---|
| `!ping` answers `Pong! (<platform>)` | | |
| `!help` lists `help` and `ping` | | |
| `!PING` (upper case) works | | |
| Messages without the prefix are ignored | | |
| Messages from other bots (for example Red) are ignored | | |
| Works on every Discord server the bot is in | | n/a |
| Bot reconnects after a network drop (disable Wi-Fi for ~30 s) | | |
| Ctrl+C shuts down cleanly | | |

Report anything odd with the log output. `RUST_LOG=debug` shows every command run.

## 5. Alerts (phase 2)

### Setup

1. **Owners.** Anyone can add, list and remove alerts; only bot owners (`BOTSU_OWNERS`) can import
   lists and toggle Shorts.
   To find your ID, run `!tube import` on each platform: the bot answers with your user ID. Put
   both IDs in `.env` (`BOTSU_OWNERS=123...,157...`) and restart the bot.
2. **Twitch (optional).** At <https://dev.twitch.tv/console/apps> → **Register Your Application**:
   any name, OAuth Redirect URL `http://localhost`, category **Chat Bot**, client type
   **Confidential**. Open it with **Manage**, copy the **Client ID**, click **New Secret** and copy
   it. Put them in `TWITCH_CLIENT_ID` / `TWITCH_CLIENT_SECRET` and restart. The log says
   `Twitch alerts enabled`; a wrong secret stops the bot at startup with a clear message.
3. **YouTube API key (recommended).** In <https://console.cloud.google.com>: create a project →
   **APIs & Services → Library** → **YouTube Data API v3** → **Enable**; then **Credentials →
   Create credentials → API key**, and restrict the key to the YouTube Data API v3. Put it in
   `YOUTUBE_API_KEY` and restart; the log says `YouTube Data API enabled`. Without it, `!tube add`
   only takes channel links or `UC...` IDs, and live streams post as ordinary uploads.
4. The database is created at `data/botsu.db` in the repo (ignored by git). Delete it to start over.

### Commands

```
!tube add <channel link | @handle | channel ID> [#channel]
!tube remove <name | @handle | channel ID> [#channel]   ← #channel: only from that channel
!tube list
!tube shorts <name | channel ID> on|off   ← Shorts are off by default
!tube message <name> [#channel] = <text>  ← custom alert text; `off` = default; no text = show it
!tube mention <name> [#channel] <@role | @user | everyone | here | off>
!twitch add <name | twitch.tv link> [#channel]
!twitch remove <name> [#channel]
!twitch list
!twitch message …   !twitch mention …     ← same as for tube
!tube import [#channel]      ← then paste the other bot's list on the following lines
!twitch import [#channel]    ← same message, list on the following lines
```

Without `#channel`, alerts go to the channel the command was typed in. The bot first posts a
short notice in the target channel; if it can't post there, nothing is saved.

**Who can do what:**

| Subcommand | Allowed |
|---|---|
| `add`, `list`, `remove`, showing `message` | anyone |
| setting `message`, `mention` a role or user | mods |
| `mention everyone` / `here` | admins |
| `import`, `tube shorts` | bot owners (`BOTSU_OWNERS`) |

`<name>` is the YouTube channel or streamer as `list` shows it (for example `My Channel`), not a
mention. Placeholders for `message`: `{name}`, `{title}`, `{url}`, `{game}` (Twitch), `{event}` ("posted a
new video", "is live on Twitch", …). Example: `!tube message Some Channel = 🪓 {name} {event}: {title} {url}`.

Deliberately lax for now; role-based permissions come with phase 4.

**Import** reads channel IDs (`UC...`) from any text, such as Red's `tube list` output, and Twitch names
from lists like `a, b and c`. It keeps the channel layout of the old bot's lists:

- Red's `tube list` headings (`Tube Subs for my-lives (111111111111111111)`) → that channel ID.
- Red's `streamalert list` headings (`#stream-alerts`, or a bare `stream-alerts`) → the channel with
  that name in this server. Lines that only say `Twitch` are ignored.
- Anything before a heading → the channel the command was typed in.
- `!tube import #channel` puts everything into that one channel instead.

Copy Red's output from Discord and paste it after the command. If the paste has `youtube:` /
`twitch:` headings, each command only reads its own section, so one paste works for both. The bot
test-posts once in each target channel; a group whose channel doesn't exist or can't be posted in
is skipped and listed. Nothing already published or already live is posted.

### Checklist

| Test | Discord | Fluxer |
|---|---|---|
| Any member can `!tube add`, `!tube list` and `!tube remove` | | `add` ✅ 2026-11-20 |
| Non-owner `!tube import` and `!tube shorts` get "Only bot owners can do that" with their ID | | |
| `!tube add @SomeChannel` replies with the channel name and latest video, no flood of old videos | | ✅ 2026-11-20 (IDs and @handle) |
| A new Short is not posted; after `!tube shorts <name> on` it is | | |
| A live stream posts "is live on YouTube"; a scheduled one posts only when it starts | live ✅ 2026-11-20 (scheduled not checked) | live ✅ 2026-11-20 (scheduled not checked) |
| `!tube add <link> #other-channel` posts the notice in the other channel | | |
| `!tube list` shows it; `!tube remove <name>` removes it | | |
| `!tube import` and `!twitch import` with the old bot's list: summary shows added / skipped | | |
| A new upload appears within ~20 min (YouTube caches the feed up to ~15 min) | ✅ 2026-11-20: twice (once 2 min after Red, once before Red) | ✅ 2026-11-20 |
| Restarting the bot doesn't repost anything | ✅ 2026-11-20 | ✅ 2026-11-20 |
| `!twitch add <streamer>` says live/offline correctly | | |
| `!tube message <name> = …` changes the next alert's text; `!tube list` shows "custom text"; `off` restores it | | set and listed ✅ 2026-11-20; next alert pending |
| `!tube mention <name> @role` pings the role on the next alert; a non-mod is refused; `everyone` needs admin | | set and listed ✅ 2026-11-20; next alert pending |
| Same source in two channels: `!tube remove <name> #one` removes only that one | | |
| A streamer going live is posted once (within ~2 min), not again on restart | ✅ 2026-11-20 (posted; restart while live not checked) | ✅ 2026-11-20 (posted; restart while live not checked) |
| `!twitch list` with names containing `_` (e.g. `Streamer_`) shows them as typed, no stray italics | | |

Quick way to see an alert without waiting for a real upload: subscribe to a channel that posts
often (news channels upload many times a day), or to a big streamer who's about to go live.

## 6. Server tools (phase 4)

### Permissions

| Level | Who |
|---|---|
| bot owner | `BOTSU_OWNERS` |
| server owner | the server's owner |
| admin | anyone with Administrator, or a role added with `!set adminrole add <role>` |
| mod | a role added with `!set modrole add <role>` |

Each level can do everything the ones below it can. `!help` lists only what you may run.

### Commands

```
!set adminrole add <role>      !set adminrole remove <role>     ← server owner
!set modrole add <role>        !set modrole remove <role>
!set roles                     ← who counts as admin and mod here
!mod cleanup messages <n>          ← mods: the last n messages (up to 1000)
!mod cleanup user <@user> <n>      ← that user's last n messages
!mod cleanup bots <n>              ← bot messages and !commands
!admin addrole <role> <@user> [@user …]      ← admins; role by mention, ID or name; up to 25 people
!admin removerole <role> <@user>             ← one person
!admin announce <#channel> <text>    ← admins
```

The bot needs **Manage Messages** (cleanup) and **Manage Roles** (addrole/removerole), and its
own role must be **above** the roles it hands out. The invite link in the log now includes both;
for the bots already in your servers, add them to the bot's role.

### Checklist

| Test | Discord | Fluxer |
|---|---|---|
| A regular member doesn't see `cleanup`/`addrole`/`set` in `!help` and gets "You need to be …" | | |
| `!set modrole add <role>`, `!set roles` shows it; a member with that role can `!mod cleanup` | | |
| `!mod cleanup messages 5` deletes 5 messages plus the command; the confirmation vanishes after 5 s | ✅ 2026-11-20 | ✅ 2026-11-20 |
| `!mod cleanup user @someone 3`, `!mod cleanup bots 10` only delete theirs | | |
| `!admin addrole @someone <role>` / `!admin removerole` work; a role above the bot's gives a clear error | refused ✅ 2026-11-20 (bot role too low) | refused ✅ 2026-11-20 (bot role too low) |
| After moving the bot's role above the test role: `!admin addrole Testing Role @a @b` gives it to both (names, no pings); `removerole Testing Role @a` takes it back (one person only); a too-high role names which role to move | | |
| `!admin announce #channel hello` posts there; a channel ID from another server is refused | | |

Fluxer's bulk-delete limits and role API are untested; note anything odd.

## 7. Moderation (phase 5)

### Commands

```
!set modlog <#channel>         ← server owner: where cases are posted (!set modlog off)
!mod warn <@user> <reason>         ← mods; the reply pings the user
!mod warnings [@user]              ← anyone: your own; mods: anyone's
!mod unwarn <case number> [reason]
!mod kick <@user> [reason]
!mod ban <@user or ID> [0-7] [reason]      ← optional days of their messages to delete
!mod tempban <@user> <duration> [reason]   ← 30m, 12h, 3d, 1w, 1d12h …
!mod unban <user ID> [reason]
!mod timeout <@user> <duration> [reason]   ← up to 28 days
!mod untimeout <@user> [reason]
!mod mute <@user> [duration] [reason]      ← mute role; no role set: a timeout (duration needed)
!mod unmute <@user> [reason]
!set muterole <role> | off     ← server owner; give the role "deny Send Messages" yourself
!set moddm on | off            ← server owner; DM members about actions against them
!mod case <n>   !mod cases <@user>   !mod reason <n> <text>   !mod bans
```

The bot needs **Kick Members**, **Ban Members** and **Moderate Members** (Discord: "Timeout
Members"), and its role must be above the people it acts on. The startup invite link now asks for
them; for the bots already in your servers, add them to the bot's role. Use a test account (or a
friend's alt) as the target.

### Checklist

| Test | Discord | Fluxer |
|---|---|---|
| `!set modlog #mod-log`; `!mod warn @test be nice` replies with case #1 and posts it in #mod-log (no ping there) | | |
| `!mod warnings` as the test account shows its own warning; `!mod warnings @someone` is refused for them | | |
| `!mod unwarn 1` removes it; `!mod warnings @test` shows none | | |
| A mod can't `!mod kick` an admin, another mod, the owner, the bot, or themselves | | |
| `!mod kick @test spam` works; the reason is in the server's audit log | | |
| `!mod timeout @test 10m` mutes them; `!mod untimeout @test` lifts it | | |
| `!mod ban <ID> 1 test` bans and deletes a day of their messages; `!mod bans` lists them; `!mod unban <ID>` | | |
| `!mod tempban @test 2m test` → the ban is lifted about 2 minutes later, with an automatic unban case | | |
| `!mod tempban @test 5m`, restart the container, wait: the ban is still lifted on time | | |
| `!mod case 2`, `!mod cases @test`, `!mod reason 2 new text` (another mod's case: admins only) | | |
| No mute role: `!mod mute @test` asks for a duration; `!mod mute @test 5m` times them out | | |
| `!set muterole Muted`; `!mod mute @test 2m` gives the role, which is taken off ~2 minutes later with an automatic unmute case | | |
| `!mod mute @test` (no duration) stays until `!mod unmute @test` | | |
| `!set moddm on`; warn/kick/ban the test account: it gets a DM with the server name and reason first. With its DMs closed, the reply says "(I couldn't DM them.)" | | |

Fluxer's moderation endpoints are untested against the instance; note anything odd.

## 8. Music (phase 6)

### Commands

```
!music play <search words or link>   ← joins your voice channel; YouTube links, playlists, other sites
!music skip    !music pause   !music resume   !music stop    !music leave
!music queue   !music now     !music remove <n>   !music clear   !music shuffle
!music volume [0-150]   ← no number: shows it; saved per server
!music join
!music test [url]    !music where    !music update     ← bot owners (tone test, voice states, yt-dlp)
```

Queue controls work for anyone in the bot's voice channel (mods from anywhere). The bot leaves
after 10 minutes without sound.

On the first start after updating, the log shows `downloading yt-dlp` and then `yt-dlp ready`
with its version. It's saved in `/data/bin`, so later starts skip the download.

### Checklist

| Test | Discord | Fluxer |
|---|---|---|
| Log shows `yt-dlp ready` with a version | | |
| `!music play never gonna give you up` joins and posts "Now playing …"; you hear it | | |
| Two more `!music play …`: "Queued … at position n"; `!music queue` lists them | | |
| `!music pause`, wait a minute, `!music resume`: it carries on where it stopped; `!music now` says "(paused)" while paused | | ✅ |
| `!music volume 30` while playing: quieter right away; the next track is quiet too; `!music volume` says 30%; still 30% after a container restart; `!music volume 100` back to normal | | |
| `!music skip` moves to the next track; the next one starts by itself when one ends | | |
| A playlist link queues its tracks ("Queued n tracks from the playlist") | | |
| `!music remove 2`, `!music shuffle`, `!music clear`, `!music stop` | | |
| Someone outside the bot's channel can't `!music skip`; a mod can | | |
| A long track (30+ min) plays to the end without cutting off | | |
| `!music update` reports the yt-dlp version | | |
| Memory while playing (Unraid's Docker page), to compare with Red + Lavalink | | ~230 MB (idle ~60 MB) |

A long pause can make YouTube drop the download (yt-dlp waits on a full pipe while paused); if a
track stops right after a long pause, that's why. A long track cutting off early is also worth
watching for.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `could not read .../.well-known/fluxer` | `FLUXER_URL` isn't your instance's web address (still the example value?), or the instance is down |
| `login check failed against ...` | Wrong `FLUXER_TOKEN` (copy the whole string, including the part before the `.`) |
| `fluxer adapter stopped (check its token)` | Fluxer rejected the token at the gateway (4004) |
| `Message Content intent is not enabled for this bot` | Turn it on in the developer portal (Bot → Privileged Gateway Intents) and **Save Changes** |
| Bot online on Discord but never answers | Message Content intent off, or no permission to send in that channel |
| `... starts with a quote` | Remove quotes from the env file |
| `Twitch rejected TWITCH_CLIENT_ID / TWITCH_CLIENT_SECRET` | Copy both again from the Twitch console; a new secret replaces the old one |
| `I can't post in <#...>` | Give the bot Send Messages (and View Channel) in that channel |
| `couldn't find a channel ID on ...` | Use the channel's link or its `UC...` ID instead of a handle |
