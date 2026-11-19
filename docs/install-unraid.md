# Running botsu on Unraid

The image is published as `caaarrrlll/botsu:latest` on Docker Hub. Nothing needs to be prepared on
the server: the container creates its folder and a settings file on first start.

## 1. Add the container

In the Unraid terminal (**Tools → Terminal**), fetch the template once:

```sh
wget -O /boot/config/plugins/dockerMan/templates-user/my-botsu.xml \
  https://raw.githubusercontent.com/Caaarrrlll/botsu/master/deploy/unraid/my-botsu.xml
```

Then **Docker → Add Container → Template: botsu → Apply**. Unraid downloads the image and starts
it. Defaults: data in `/mnt/user/appdata/botsu`, running as `nobody:users` (99:100, changeable under
**Show more settings** as `PUID`/`PGID`). No ports are needed: the bot only connects outwards.

## 2. Fill in the settings

On first start the bot creates `/mnt/user/appdata/botsu/botsu.env` and logs:

```
WARN botsu: created a settings file file=/data/botsu.env
WARN botsu: no platform configured: put DISCORD_TOKEN and/or FLUXER_TOKEN in this file ...
```

Edit that file, over the network share (`\\<unraid-host>\appdata\botsu\botsu.env`) or in the terminal
(`nano /mnt/user/appdata/botsu/botsu.env`). Every setting is explained in the file; the important
ones:

| Setting | |
|---|---|
| `DISCORD_TOKEN` and/or `FLUXER_TOKEN` | at least one; see [testing.md](testing.md) for creating the bots |
| `FLUXER_URL` | your Fluxer address, for a self-hosted instance |
| `BOTSU_PREFIX` | `!` while Red still runs on the same servers |
| `BOTSU_OWNERS` | your user IDs, for adding alerts |
| `YOUTUBE_API_KEY`, `TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET` | for alerts |

**No quotes** around values. Save, and within 30 seconds the bot starts by itself. After that,
**restart the container** to apply further changes to the file.

The file is readable only by its owner and the `users` group, so the tokens aren't world-readable.

**Stop the bot on your PC** before the server one connects, otherwise both answer every command.

## 3. Check it

**Docker** tab → botsu icon → **Logs**, or `docker logs -f botsu`. Expected:

```
INFO botsu: starting botsu 0.1.0 prefix="!" data_dir=/data
INFO botsu: YouTube Data API enabled (@handles, live/upcoming detection)
INFO botsu: Twitch alerts enabled
INFO botsu_fluxer: connected to Fluxer user=botsu
INFO botsu_discord: connected to Discord user=... guilds=N
```

Then `!ping` on Discord and Fluxer. Memory use: `docker stats botsu --no-stream`. If it stops,
the log's last line names the setting to fix.

## Moving your local test database (optional)

To keep subscriptions made while testing on your PC: stop the container and the local bot, copy
`data/botsu.db` from the repo (plus `botsu.db-wal` / `botsu.db-shm` if they exist) to
`\\<unraid-host>\appdata\botsu\`, and start the container. Ownership is fixed automatically on start.

## Updating

**Docker** tab → **Check for Updates** → **Apply Update**, or:

```sh
docker pull caaarrrlll/botsu:latest   # then Apply Update / recreate the container
```

Settings and subscriptions live in `/mnt/user/appdata/botsu`, so they survive updates.

## Keeping secrets elsewhere (optional)

Settings can also come from environment variables, which override the file: add
`--env-file /mnt/user/appdata/secrets/botsu.env` (a root-only file you create) to **Extra
Parameters**. Editing that file needs the container recreated (**Apply**), not just restarted.

## Unraid gotcha

If a container's **first** start fails, Docker can leave it with no network
(`docker inspect botsu` shows `"Networks": {}`). Fix: remove the container and **Apply** the
template again.
