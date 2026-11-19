# Running botsu on Unraid

Interim guide until phase 3 publishes the image to Docker Hub and adds an Unraid template. Until
then the image is built yourself and the container is created from the Unraid terminal
(**Tools → Terminal**, or SSH as root). Commands below are for that terminal.

## 1. Secrets file

All settings live in one root-only env file, outside the container and outside git:

```sh
mkdir -p /mnt/user/appdata/secrets
nano /mnt/user/appdata/secrets/botsu.env      # paste your settings, see below
chmod 600 /mnt/user/appdata/secrets/botsu.env
```

Use the same keys as your local `.env` (see [`.env.example`](../.env.example)):
`DISCORD_TOKEN`, `FLUXER_TOKEN`, `FLUXER_URL`, `BOTSU_PREFIX`, `BOTSU_OWNERS`, `YOUTUBE_API_KEY`,
`TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET`, `RUST_LOG`.

- **No quotes** around values: Docker keeps them as part of the value (the bot refuses tokens
  that start with a quote).
- Leave out `BOTSU_DATA_DIR`: the image already uses `/data`.
- Editing this file later needs the container **recreated** (step 4 again), not just restarted.

## 2. Data folder

The database (subscriptions, what's been posted) goes in appdata. Unraid's convention is to run
containers as `nobody:users` (99:100), so give that user the folder:

```sh
mkdir -p /mnt/user/appdata/botsu
chown 99:100 /mnt/user/appdata/botsu
```

**Moving your local test database** (optional, keeps subscriptions you made while testing):
stop the local bot first, then copy `data/botsu.db` from the repo to `/mnt/user/appdata/botsu/`
(for example over the network share `\\TOWER\appdata\botsu`), and run the `chown` again.
If `botsu.db-wal` and `botsu.db-shm` exist next to it, copy them too.

## 3. Get the image

Pick one.

**A. Build on Unraid straight from GitHub** (needs the latest commits pushed):

```sh
docker build -t botsu:latest https://github.com/Caaarrrlll/botsu.git#master
```

The first build takes 10–20 minutes (Rust compile plus the libwebrtc download); later builds
reuse the cache.

**B. Build on your PC and copy it over** (no push needed):

```powershell
docker build -t botsu:latest .
docker save botsu:latest -o botsu.tar
```

Copy `botsu.tar` to Unraid (for example to `\\TOWER\appdata\`), then on Unraid:

```sh
docker load -i /mnt/user/appdata/botsu.tar && rm /mnt/user/appdata/botsu.tar
```

## 4. Create the container

**Stop the bot on your PC first**, otherwise both answer every command.

```sh
docker run -d --name botsu --restart unless-stopped \
  --user 99:100 \
  -v /mnt/user/appdata/botsu:/data \
  --env-file /mnt/user/appdata/secrets/botsu.env \
  botsu:latest
```

No ports are published: the bot only makes outgoing connections. The container shows up in
Unraid's **Docker** tab (start/stop/logs work there). Created this way it has no template, so
settings are changed by editing the env file and running this step again.

**Or with the template** (adds the icon and an Edit page in the Docker tab): copy
[`deploy/unraid/botsu.xml`](../deploy/unraid/botsu.xml) to
`/boot/config/plugins/dockerMan/templates-user/my-botsu.xml`, then **Docker → Add Container →
Template: botsu** → **Apply**. It sets the same `--user`, `--env-file` and `/data` mapping as the
command above. The template uses the local image `botsu:latest` from step 3; if Unraid tries to
pull it from Docker Hub and fails, use the `docker run` command instead until phase 3 publishes
the image. The icon is loaded from GitHub, so it appears once `assets/monkasS.png` is pushed.

## 5. Check it

```sh
docker logs -f botsu          # Ctrl+C stops following, not the bot
```

Expected within a few seconds:

```
INFO botsu: starting botsu 0.1.0 prefix="!" data_dir=/data
INFO botsu_core::storage: applied database migration version=1      (first start only)
INFO botsu: YouTube Data API enabled (@handles, live/upcoming detection)
INFO botsu: Twitch alerts enabled
INFO botsu_fluxer: Fluxer token OK user=botsu ...
INFO botsu_fluxer: connected to Fluxer user=botsu
INFO botsu_discord: connected to Discord user=... guilds=N
```

Then `!ping` on Discord and Fluxer. Memory use: `docker stats botsu --no-stream`.

If it exits, `docker logs botsu` shows why; the messages name the setting to fix. A permissions
error about `/data` means step 2's `chown` is missing.

## Updating

```sh
docker build -t botsu:latest https://github.com/Caaarrrlll/botsu.git#master   # or option B
docker rm -f botsu
# then step 4 again
```

Subscriptions survive this: they're in `/mnt/user/appdata/botsu`, not in the container.

## Unraid gotcha

If a container's **first** start fails (for example a typo in the run command), Docker can leave
it with no network (`docker inspect botsu` shows `"Networks": {}`). Fix: `docker rm -f botsu` and
run step 4 again.
