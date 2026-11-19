# Cutover: replacing Red with botsu (phase 7)

botsu runs next to Red on the Discord servers with `!`; Red keeps `$`. This runbook moves every
server over in one short window, keeps Red recoverable for a week, and then removes it.
Fluxer never had Red, so it only gets the new prefix.

Do all of it in one sitting, server by server. An alert subscribed in both bots posts twice, and
one in neither is missed. Keeping the window between "import into botsu" and "stop Red" short
avoids both.

## 1. Before the window (botsu still on `!`)

Per Discord server, with Red still running:

| Step | Command (botsu) | Red equivalent, to copy from |
|---|---|---|
| Admin roles | `!set adminrole add <role>` | `$set showsettings` (Admin roles) |
| Mod roles | `!set modrole add <role>` | `$set showsettings` (Mod roles) |
| Check | `!set roles` | |
| Mod-log channel | `!set modlog #channel` (the same channel Red uses is fine) | the channel Red's cases appear in (don't run `$modlogset modlog` without a channel: it may clear Red's setting) |
| Bot permissions | botsu's role needs Manage Messages, Manage Roles, Kick, Ban, Moderate Members, Connect, Speak, and must sit above the roles it manages | |

Also:

- **Bot owners:** your IDs on both platforms are in `BOTSU_OWNERS` (`!tube import` answers with
  your ID if not).
- **Temp-bans running in Red:** Red lifts its own temp-bans, and stops doing so once it's
  stopped. Note the people and end times from Red's mod-log. After the switch, unban them by hand
  when due (`$mod unban <id>`), or unban and re-issue with
  `$mod tempban <id> <remaining time> <reason>` so botsu schedules the unban.
- **Copy the alert lists**, without changing anything in Red yet: `$tube list` and
  `$streamalert list` in each alert channel. Keep the output; it's pasted into botsu in step 2.
- **Write down memory** for the record: Unraid's Docker page, or
  `docker stats --no-stream red-discordbot botsu`.

## 2. The window

1. **Import alerts into botsu** (bot owner, per server):
   `!tube import` and `!twitch import` with the copied lists on the following lines (or
   `!tube import #channel` to send everything to one channel). The summary says added / already
   there / skipped. Anything already published or live is marked as seen, so nothing old is posted.
   `!tube list` / `!twitch list` to check.
2. **Stop Red** (Unraid: stop the `red-discordbot` container). Don't remove it yet. Lavalink is
   managed by Red and stops with it.
3. **Give botsu `$`:** set `BOTSU_PREFIX=$` where it's set now (the Unraid template's variable,
   or `/data/botsu.env`; the template's value wins). Removing the key also gives `$`, the default.
   Restart botsu.
4. **Smoke test on each server:** `$ping`, `$help`, `$music play <something>`, `$mod cases`,
   `$tube list`. On Fluxer the prefix is now `$` too.
5. Tell the members: the commands are grouped now (`$mod ban`, `$admin addrole`,
   `$music play`), and `$help` lists them.

## 3. The week after

- Watch the first real upload and stream go out once, from botsu only.
- **Rollback**, if something important is missing: stop botsu, set its prefix back to `!`,
  start botsu and Red. Red's data is untouched.
- **Then remove Red:** delete the `red-discordbot` container and its appdata
  (`/mnt/user/appdata/redbot`, back it up first if you want the old cases), and kick Red's bot
  user from the servers. Write down memory again.
- **Update the server notes:** container template (`caaarrrlll/botsu:latest`, `/data` mapping,
  PUID/PGID 99:100), where the env file is (`/data/botsu.env` and the template variables), the
  update procedure (push to master, CI publishes; Unraid: Check for Updates → Apply Update), and
  that `redbot-youtube-fix.md` no longer applies.

## Not carried over

- **Red's mod cases.** They stay readable in the mod-log channel's history; botsu starts its case
  numbers at 1.
- **Red's settings for unused cogs** (aimage, crier, customcom, image, trivia: already unloaded).
