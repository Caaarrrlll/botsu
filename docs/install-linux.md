# Running botsu natively on Linux (systemd)

Without containers: x86_64, systemd, glibc 2.35 or newer (Debian 12, Ubuntu 22.04, current
Fedora and Arch). With Docker it's simpler: [install-docker.md](install-docker.md). Windows:
[install-windows.md](install-windows.md).

## 1. Get botsu

Pick one. Prebuilt files are on the [releases page](https://github.com/Caaarrrlll/botsu/releases).

### a. Debian / Ubuntu: the .deb

```sh
sudo apt install ./botsu_*_amd64.deb
```

It installs `/usr/bin/botsu`, the systemd unit and the runtime libraries, and recommends ffmpeg.
It doesn't start the bot: it needs a token first (step 2). Updating: install the newer `.deb`
the same way, then `sudo systemctl restart botsu`.

### b. Any distribution: the .tar.gz

```sh
tar -xzf botsu-*-linux-x86_64.tar.gz
cd botsu-*-linux-x86_64
sudo install -m 0755 botsu /usr/local/bin/botsu
sudo install -m 0644 botsu.service /etc/systemd/system/botsu.service
```

The runtime libraries it needs:

| Distribution | Command |
|---|---|
| Debian / Ubuntu | `sudo apt install -y ca-certificates libglib2.0-0 libva2 libva-drm2 libopus0` |
| Fedora | `sudo dnf install -y ca-certificates glib2 libva opus` |
| Arch | `sudo pacman -S --needed ca-certificates glib2 libva opus` |

Updating: replace `/usr/local/bin/botsu` with the newer binary, then `sudo systemctl restart botsu`.

### c. Build from source

Needs a C/C++ compiler, pkg-config and the development files for glib, libva and opus. The first
build takes 10–20 minutes (it downloads a prebuilt libwebrtc).

| Distribution | Build dependencies |
|---|---|
| Debian / Ubuntu | `sudo apt install -y build-essential pkg-config libglib2.0-dev libva-dev libopus-dev git curl` |
| Fedora | `sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config glib2-devel libva-devel opus-devel git curl` |
| Arch | `sudo pacman -S --needed base-devel pkgconf glib2 libva opus git curl` |

Then Rust (from rustup; distribution packages can be too old) and the build:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

git clone https://github.com/Caaarrrlll/botsu.git
cd botsu
cargo build --release -p botsu
sudo install -m 0755 target/release/botsu /usr/local/bin/botsu
sudo install -m 0644 deploy/systemd/botsu.service /etc/systemd/system/botsu.service
```

On Arch, `sudo pacman -S rustup && rustup default stable` works instead of the curl line.

Updating:

```sh
cd botsu && git pull
cargo build --release -p botsu
sudo install -m 0755 target/release/botsu /usr/local/bin/botsu
sudo systemctl restart botsu
```

## 2. Settings

```sh
sudo install -d -m 0700 /etc/botsu
sudo install -m 0600 .env.example /etc/botsu/botsu.env
sudoedit /etc/botsu/botsu.env      # at least DISCORD_TOKEN or FLUXER_TOKEN, no quotes
```

With the `.deb`, the example is at `/usr/share/doc/botsu/botsu.env.example`. Every setting is
explained in the file. Leave `BOTSU_DATA_DIR` out: the service sets it. Creating the Discord and
Fluxer bots: [testing.md](testing.md), sections 1 and 2.

## 3. Service

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now botsu
journalctl -u botsu -f
```

The log shows both platforms connecting and an invite link for each. The unit runs the bot as a
throwaway user (`DynamicUser`) with a hardened sandbox; its database lives in `/var/lib/botsu`
(systemd creates it). Back that folder up if you care about the alert subscriptions.

Without `/etc/botsu/botsu.env`, the bot creates `/var/lib/botsu/botsu.env` on first start and
waits for a token there instead (edit it as root, then it starts within 30 seconds).

Changes to `/etc/botsu/botsu.env` need `sudo systemctl restart botsu`.

## Music

Music needs three programs on the PATH of the user botsu runs as:

- **ffmpeg**:

  | Distribution | Command |
  |---|---|
  | Debian / Ubuntu | `sudo apt install ffmpeg` (the `.deb` recommends it) |
  | Fedora | Fedora's own `ffmpeg-free` lacks some codecs; use RPM Fusion's: enable RPM Fusion, then `sudo dnf swap ffmpeg-free ffmpeg --allowerasing` |
  | Arch | `sudo pacman -S ffmpeg` |

- **Deno 2.3+**: yt-dlp needs a JavaScript runtime for YouTube. Arch: `sudo pacman -S deno`.
  Elsewhere: `curl -fsSL https://deno.land/install.sh | sh`, then make sure `deno` is on botsu's
  PATH (for example copy `~/.deno/bin/deno` to `/usr/local/bin`).
- **yt-dlp**: nothing to do. botsu downloads the standalone build into `<data dir>/bin` on first
  start and updates it daily (`!music update` updates it right away). To use your own copy
  instead, set `BOTSU_YTDLP=/path/to/yt-dlp` in `botsu.env` and keep it updated yourself.
