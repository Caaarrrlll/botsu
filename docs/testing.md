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
   https://discord.com/oauth2/authorize?client_id=APP_ID&scope=bot&permissions=68608
   ```

   `68608` = View Channels + Send Messages + Read Message History, enough for phase 1.
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
   https://fluxer.example.com/oauth2/authorize?client_id=APP_ID&scope=bot&permissions=68608
   ```

   Open it while logged in, pick the community and approve. You need Manage Community (Manage
   Guild) or Administrator there. With non-zero `permissions`, Fluxer creates a role named after
   the bot. `68608` is View Channels + Send Messages + Read Message History *if* Fluxer uses
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

1. **Owners.** Only user IDs in `BOTSU_OWNERS` can add or remove alerts. To find yours, run
   `!tube add x` on each platform: the bot answers with your user ID. Put both IDs in `.env`
   (`BOTSU_OWNERS=123...,157...`) and restart the bot.
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
!tube remove <name | @handle | channel ID>
!tube list
!tube shorts <name | channel ID> on|off   ← Shorts are off by default
!twitch add <name | twitch.tv link> [#channel]
!twitch remove <name>
!twitch list
!tube import [#channel]      ← then paste the other bot's list on the following lines
!twitch import [#channel]    ← same message, list on the following lines
```

Without `#channel`, alerts go to the channel the command was typed in. The bot first posts a
short notice in the target channel; if it can't post there, nothing is saved.

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
| Non-owner gets "Only bot owners can do that" with their ID | | |
| `!tube add @SomeChannel` replies with the channel name and latest video, no flood of old videos | | |
| A new Short is not posted; after `!tube shorts <name> on` it is | | |
| A live stream posts "is live on YouTube"; a scheduled one posts only when it starts | | |
| `!tube add <link> #other-channel` posts the notice in the other channel | | |
| `!tube list` shows it; `!tube remove <name>` removes it | | |
| `!tube import` and `!twitch import` with the old bot's list: summary shows added / skipped | | |
| A new upload appears within ~20 min (YouTube caches the feed up to ~15 min) | | |
| Restarting the bot doesn't repost anything | | |
| `!twitch add <streamer>` says live/offline correctly | | |
| A streamer going live is posted once (within ~2 min), not again on restart | | |

Quick way to see an alert without waiting for a real upload: subscribe to a channel that posts
often (news channels upload many times a day), or to a big streamer who's about to go live.

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
