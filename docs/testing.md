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

1. Find the API URL: open `https://<your-instance>/.well-known/fluxer` in a browser and look at
   `endpoints.api`. `FLUXER_API_URL` is that value including the `/v1` suffix (for example
   `https://fluxer.example.com/api/v1`). While there, note `features.voice_enabled`: phase 0
   needs it to be `true` for music on Fluxer.
2. Create a bot application in the Fluxer web app (user settings → applications / developer
   section; Fluxer follows Discord's model) and copy its bot token → `FLUXER_TOKEN`.
3. Add the bot to a test community (guild) and give it permission to read and send in a channel.

If your instance's UI differs from this, note what you found here so the next person has it.

## 3. Run from source

```sh
cp .env.example .env      # fill in DISCORD_TOKEN, FLUXER_TOKEN, FLUXER_API_URL, BOTSU_PREFIX
set -a; . ./.env; set +a  # Git Bash / Linux. PowerShell: see below
cargo run -p botsu
```

PowerShell:

```powershell
Get-Content .env | Where-Object { $_ -match '^\s*[^#].*=' } | ForEach-Object {
  $k, $v = $_ -split '=', 2; Set-Item "env:$($k.Trim())" $v.Trim() }
cargo run -p botsu
```

Expected log lines:

```
INFO botsu: starting botsu 0.1.0 prefix="!"
INFO botsu: Fluxer enabled api_url=https://fluxer.example.com/api/v1
INFO botsu_fluxer: Fluxer instance discovered ... voice_enabled=true self_hosted=true
INFO botsu_fluxer: Fluxer token OK user=botsu-dev
INFO botsu_fluxer: connected to Fluxer user=botsu-dev
INFO botsu_discord: connected to Discord user=botsu-dev guilds=3
```

The `/.well-known/fluxer` warning is not fatal: fluxer-rust looks for it next to the API path,
which some instances don't serve. The token check after it is what matters.

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

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `login check failed against ...` | Wrong `FLUXER_API_URL` (missing `/v1`?) or token |
| `fluxer adapter stopped (check its token)` | Fluxer rejected the token at the gateway (4004) |
| `Disallowed intent(s)` from Discord | Message Content intent not enabled in the developer portal |
| Bot online on Discord but never answers | Message Content intent off, or no permission to send in that channel |
| `... starts with a quote` | Remove quotes from the env file |
