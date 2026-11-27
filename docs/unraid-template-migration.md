# Handover: Unraid template migration (log levels)

For whoever updates the botsu container on Unraid. Written 2026-11-22. The current template is
[`deploy/unraid/my-botsu.xml`](../deploy/unraid/my-botsu.xml); this note covers moving the live
server from the template migrated on 2026-11-20 to that one.

## What changed

One field, since the 2026-11-20 migration:

| Before | Now |
|---|---|
| **Log level** (`RUST_LOG`), under *Show more settings*. Blank meant `info,botsu=debug`. | **Log levels** (`BOTSU_LOG`), on the main page. Blank means `ERROR,WARN,INFO`. |

`BOTSU_LOG` takes any combination of `ERROR`, `WARN`, `INFO`, `DEBUG`, `TRACE`, or `ALL`,
comma-separated, in any case. Only the listed levels are printed:

| Value | Prints |
|---|---|
| *(blank)* | errors, warnings and info: the normal amount |
| `ERROR` | errors only |
| `WARN,ERROR` | warnings and errors |
| `DEBUG,INFO,WARN,ERROR` | everything useful, including a line for every command run |
| `DEBUG,ERROR` | debug lines and errors, nothing in between |
| `ALL` | every level, including `TRACE` |

The levels apply to botsu's own lines. The libraries underneath (Discord, Fluxer, voice) only
ever print their warnings and errors, so `ALL` doesn't flood the log.

`RUST_LOG` still works as an expert override, but it's no longer in the template. If it's set,
it wins over `BOTSU_LOG` from the same place.

Nothing else in the template changed. The image needs no other settings for the features added
since (alert embeds, `!invite`, settings export/import, `!set embeds`).

## Before you start

1. **Check the old field.** Docker → botsu → **Edit** → *Show more settings* → **Log level**.
   - If it's **blank**, there's nothing to carry over.
   - If it has a value, decide what you want in **Log levels** instead (table above).
2. **Check `botsu.env`.** Open `/mnt/user/appdata/botsu/botsu.env` and look for a line starting
   with `RUST_LOG=`. Older versions put `RUST_LOG=info,botsu=debug` there. botsu used to ignore
   it; it reads it now.
   - A **Log levels** value in the template wins over that line.
   - With **Log levels** blank, that old line decides the logging, and you won't get the new
     default.
   - Delete the line, or put `#` in front of it, unless you want it.
3. **Check log rotation.** Settings → Docker → **Docker log rotation** should be **Yes**. botsu
   writes no log file of its own: Docker keeps what it prints, and without rotation that file
   grows without limit.

## Option A: change the field by hand (keeps everything else)

Quickest, and no other field is touched.

1. Docker → botsu → **Edit**.
2. *Show more settings* → **Log level** (`RUST_LOG`): **Remove** it.
   - Leaving it with a value would override **Log levels**.
3. **Add another Path, Port, Variable, Label or Device** → Config Type **Variable**:
   - Name: `Log levels`
   - Key: `BOTSU_LOG`
   - Value: blank for the default, or your choice from the table above
   - Description: optional; copy it from the template if you like
4. **Apply**.

The field description on Unraid will differ from the repo's until the next full template refresh.
That's cosmetic.

## Option B: replace the template file (new descriptions too)

Unraid saves the values you typed into the template fields in the same file, so replacing it
clears them. Note them first.

1. **Note your values.** Docker → botsu → **Edit**, *Show more settings*, and write down every
   field you filled in: tokens, Fluxer address, prefix, bot owners, Invite as admin, YouTube API
   key, Twitch ID and secret, and anything advanced. Masked fields (tokens, keys, secret) show
   their value when you click the eye icon. Keep this note somewhere safe and delete it after.
   - If your tokens are in `botsu.env` instead, they're safe: the file isn't touched.
2. **Back up the current template**, in the Unraid terminal (**Tools → Terminal**):

   ```sh
   cp /boot/config/plugins/dockerMan/templates-user/my-botsu.xml \
      /boot/config/plugins/dockerMan/templates-user/my-botsu.xml.bak
   ```

3. **Download the new one:**

   ```sh
   wget -O /boot/config/plugins/dockerMan/templates-user/my-botsu.xml \
     https://raw.githubusercontent.com/Caaarrrlll/botsu/master/deploy/unraid/my-botsu.xml
   ```

4. Docker → botsu → **Edit**. Enter the values from step 1 again, set **Log levels**, and
   **Apply**.

## Check it worked

1. Docker → botsu → **Logs**. The start shows both platforms connecting and an invite link for
   each.
2. In a test channel, `!ping` and `!invite` answer.
3. With **Log levels** blank, no `DEBUG` lines appear. Set it to `DEBUG,INFO,WARN,ERROR`, Apply,
   run `!ping`, and a `running command` line appears. Set it back afterwards.
4. With **Invite as admin** set to `true`, the invite links end in `permissions=8`.

## If something goes wrong

- **Option A:** Edit again, remove `BOTSU_LOG`, and add back `RUST_LOG` with its old value.
- **Option B:** restore the backup and Apply:

  ```sh
  cp /boot/config/plugins/dockerMan/templates-user/my-botsu.xml.bak \
     /boot/config/plugins/dockerMan/templates-user/my-botsu.xml
  ```

- **No log output at all:** check **Log levels** for a typo. Unknown words are ignored with a
  warning; a value with no known word falls back to the default.
- **Bot doesn't start after Apply:** see "Unraid gotcha" in [install-unraid.md](install-unraid.md)
  (a failed first start can leave the container without a network).

Your data, alert subscriptions and `botsu.env` live in `/mnt/user/appdata/botsu`. Neither option
touches them.

## Field reference (current template)

Main page:

| Field | Variable | Blank means | Notes |
|---|---|---|---|
| Data | `/data` path | `/mnt/user/appdata/botsu` | database and `botsu.env` |
| Discord token | `DISCORD_TOKEN` | Discord off | masked; needs Message Content Intent |
| Fluxer token | `FLUXER_TOKEN` | Fluxer off | masked |
| Fluxer address | `FLUXER_URL` | fluxer.app | your instance's web address |
| Command prefix | `BOTSU_PREFIX` | `$` | `!` while Red still uses `$` |
| Bot owners | `BOTSU_OWNERS` | nobody | user IDs, comma-separated, any platform |
| Invite as admin | `BOTSU_INVITE_ADMIN` | `false` | `true`: invite links ask for Administrator |
| YouTube API key | `YOUTUBE_API_KEY` | links and IDs only, no live detection | masked |
| Twitch client ID | `TWITCH_CLIENT_ID` | Twitch off | set both Twitch fields |
| Twitch client secret | `TWITCH_CLIENT_SECRET` | Twitch off | masked |
| **Log levels** | `BOTSU_LOG` | `ERROR,WARN,INFO` | **new**, replaces Log level (`RUST_LOG`) |

*Show more settings*:

| Field | Variable | Blank means | Notes |
|---|---|---|---|
| Fluxer API URL | `FLUXER_API_URL` | discovered from the address | only if discovery fails |
| YouTube poll interval | `YOUTUBE_POLL_SECONDS` | 300 | minimum 60 |
| Twitch poll interval | `TWITCH_POLL_SECONDS` | 120 | minimum 30 |
| yt-dlp path | `BOTSU_YTDLP` | botsu's own copy, updated daily | |
| PUID | `PUID` | 99 (nobody) | |
| PGID | `PGID` | 100 (users) | |

Any of these can also live in `botsu.env` instead. A value in the template wins over the file.

## Later change: several Fluxer instances (experimental image)

One new field, **Fluxer official token** (`FLUXER_OFFICIAL_TOKEN`): the bot's token from
fluxer.com, to join servers on the official Fluxer next to your self-hosted instance. **Fluxer
address** and **Fluxer token** now take comma-separated lists (same order), but a single address
and token work as before, so nothing else needs changing. Add the field by hand (Option A above)
or replace the template (Option B).

