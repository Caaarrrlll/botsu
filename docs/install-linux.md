# Running botsu natively on Linux (systemd)

Without containers, on a Debian/Ubuntu-style server. Tested assumptions: x86_64, systemd, glibc.
With Docker it's simpler: [install-docker.md](install-docker.md).

## 1. Build

botsu is built from source (no prebuilt binaries yet). On the server, or on another machine with
the same distribution:

```sh
sudo apt install -y build-essential pkg-config libglib2.0-dev libva-dev git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

git clone https://github.com/Caaarrrlll/botsu.git
cd botsu
cargo build --release -p botsu        # first build: 10–20 minutes (downloads libwebrtc)
sudo install -m 0755 target/release/botsu /usr/local/bin/botsu
```

If you build elsewhere, the server needs the runtime libraries:
`sudo apt install -y ca-certificates libglib2.0-0 libva2 libva-drm2`.

## 2. Settings

```sh
sudo install -d -m 0700 /etc/botsu
sudo install -m 0600 .env.example /etc/botsu/botsu.env
sudoedit /etc/botsu/botsu.env      # at least DISCORD_TOKEN or FLUXER_TOKEN, no quotes
```

Every setting is explained in the file. Leave `BOTSU_DATA_DIR` out: the service sets it.
Creating the Discord and Fluxer bots: [testing.md](testing.md), sections 1 and 2.

## 3. Service

```sh
sudo install -m 0644 deploy/systemd/botsu.service /etc/systemd/system/botsu.service
sudo systemctl daemon-reload
sudo systemctl enable --now botsu
journalctl -u botsu -f
```

The log shows both platforms connecting and an invite link for each. The unit runs the bot as a
throwaway user (`DynamicUser`) with a hardened sandbox; its database lives in `/var/lib/botsu`
(systemd creates it). Back that folder up if you care about the alert subscriptions.

Without `/etc/botsu/botsu.env`, the bot creates `/var/lib/botsu/botsu.env` on first start and
waits for a token there instead (edit it as root, then it starts within 30 seconds).

## Updating

```sh
cd botsu && git pull
cargo build --release -p botsu
sudo install -m 0755 target/release/botsu /usr/local/bin/botsu
sudo systemctl restart botsu
```

Changes to `/etc/botsu/botsu.env` need `sudo systemctl restart botsu`.

## Later: music

Phase 6 (music) will add `ffmpeg` and `yt-dlp` to the requirements; this guide gets those steps
then.
