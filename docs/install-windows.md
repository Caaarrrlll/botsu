# Running botsu on Windows

For a home PC or a Windows server. Docker or Linux are better for running it around the clock
([install-docker.md](install-docker.md), [install-linux.md](install-linux.md)), but the Windows
build works the same way.

## 1. Get botsu

Download `botsu-<version>-windows-x86_64.zip` from the
[releases page](https://github.com/Caaarrrlll/botsu/releases) and unzip it somewhere permanent,
for example `C:\botsu`.

## 2. First start and settings

Start `botsu.exe` from that folder (double-click, or `.\botsu.exe` in a terminal there). On the
first start it creates `data\botsu.env` next to it and waits for a token. Open that file, set at
least `DISCORD_TOKEN` or `FLUXER_TOKEN` (no quotes), save: the bot starts within 30 seconds.
Every setting is explained in `.env.example`. Creating the Discord and Fluxer bots:
[testing.md](testing.md), sections 1 and 2.

The `data` folder holds the database too; back it up if you care about the subscriptions. Set
`BOTSU_DATA_DIR` to keep it elsewhere.

## Music

Install ffmpeg and Deno (yt-dlp needs a JavaScript runtime for YouTube), then open a **new**
terminal so they're on the PATH:

```powershell
winget install Gyan.FFmpeg
winget install DenoLand.Deno
```

yt-dlp needs nothing: botsu downloads `yt-dlp.exe` into `data\bin` and updates it daily.

## Running it in the background

botsu is a console program; closing its window stops it. To start it with Windows without
logging in, use Task Scheduler: **Create Task** → *Run whether user is logged on or not*,
trigger *At startup*, action *Start a program* `C:\botsu\botsu.exe` with **Start in** set to
`C:\botsu` (so it finds its `data` folder). Under *Settings*, untick *Stop the task if it runs
longer than*.

## Updating

Stop it, replace `botsu.exe` with the one from the newer zip, start it again. `data` stays.

## Building it yourself

Needs Rust (rustup, MSVC toolchain: install the Visual Studio Build Tools with "Desktop
development with C++" when rustup asks) and CMake (`winget install Kitware.CMake`; opus is built
from source):

```powershell
git clone https://github.com/Caaarrrlll/botsu.git
cd botsu
cargo build --release -p botsu   # target\release\botsu.exe
```
