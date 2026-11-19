# Running botsu with Docker

For any Docker host. Unraid has its own guide with a template: [install-unraid.md](install-unraid.md).
Without containers: [install-linux.md](install-linux.md).

Image: [`caaarrrlll/botsu`](https://hub.docker.com/r/caaarrrlll/botsu), `linux/amd64`.

| Tag | |
|---|---|
| `latest` | newest build of `master` (every push, after tests pass) |
| `sha-<commit>` | one exact commit, for pinning or rolling back |
| `1.2.3`, `1.2` | releases, once versions are tagged |

## Docker Compose

```sh
mkdir botsu && cd botsu
curl -O https://raw.githubusercontent.com/Caaarrrlll/botsu/master/deploy/docker-compose.yml
# edit PUID/PGID in docker-compose.yml to your user (`id -u`, `id -g`)
docker compose up -d
docker compose logs -f
```

## docker run

```sh
docker run -d --name botsu --restart unless-stopped \
  -e PUID=1000 -e PGID=1000 \
  -v "$PWD/botsu-data:/data" \
  caaarrrlll/botsu:latest
```

## Settings

On first start the bot creates `botsu.env` in the data folder and logs:

```
WARN botsu: created a settings file file=/data/botsu.env
WARN botsu: no platform configured: set DISCORD_TOKEN and/or FLUXER_TOKEN in this file, ...
```

Fill in at least one token (every setting is explained in the file, see also
[`.env.example`](../.env.example)), **without quotes**. The bot starts within 30 seconds; restart
the container to apply later changes.

Alternatively pass settings as environment variables (`-e KEY=value`, `--env-file`, or
`environment:` / `env_file:` in Compose). Those win over the file; blank values are ignored.
Changing them needs the container recreated (`docker compose up -d`), not just restarted.

Creating the Discord and Fluxer bots: [testing.md](testing.md), sections 1 and 2. Once connected,
the log prints an invite link for each platform.

## Data and permissions

`/data` holds `botsu.env` and the SQLite database (alert subscriptions, what's been posted).
Keep it on a volume or host folder; back it up if you care about the subscriptions.

The container starts as root only to create `/data` and hand it to `PUID:PGID` (default 99:100,
Unraid's nobody:users), then runs the bot as that user. With `--user` instead, it skips that step
and the folder must already be writable by that user.

## Updating

```sh
docker compose pull && docker compose up -d
# or: docker pull caaarrrlll/botsu:latest, then remove and re-run the container
```

To roll back, use a `sha-<commit>` tag from
[Docker Hub](https://hub.docker.com/r/caaarrrlll/botsu/tags) instead of `latest`.
