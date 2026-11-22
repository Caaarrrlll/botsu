# Resource usage

Memory and CPU of the botsu container, measured on the live Unraid server (Docker page). Add a row
whenever there's a new situation worth knowing: more servers, more people, several servers playing
music at once.

## Measurements

| Date | Situation | Memory | CPU | Notes |
|---|---|---|---|---|
| 2026-11-19 | idle | ~60 MB | | first measurement |
| 2026-11-19 | Fluxer playing (1 server) | ~230 MB | | |
| 2026-11-22 | idle, after 24 h mostly idle | 34 MiB | 0% | Red at the same time: 376 MiB (Red alone, Lavalink not counted) |
| 2026-11-22 | Discord playing (1 server) | 112.5 MiB | 0.06% | |
| 2026-11-22 | Discord and Fluxer playing (1 server each) | 251.8 MiB | 0.17% | |

For comparison: Red plus Lavalink used 520 MB to 1.2 GB.

## What the memory is made of

Estimated from the numbers above; the split per process hasn't been measured yet.

| Part | When | Roughly |
|---|---|---|
| botsu itself | always | ~35–60 MB |
| yt-dlp + ffmpeg | **per server playing**, for the whole song | ~60–70 MB |
| Discord voice (songbird) | per call | a few MB |
| Fluxer voice (LiveKit/WebRTC) | **once**, from the first Fluxer call | ~70 MB |

- Each extra server playing music adds roughly 60–70 MB, mostly yt-dlp: it keeps running for the
  whole song because it streams the audio into ffmpeg (YouTube often refuses bare stream links).
- When a song ends, both processes exit and their memory is freed.
- Fluxer's LiveKit part is paid once, however many Fluxer servers play.
- Alerts, moderation and commands add next to nothing while idle.

## Still to measure

- [ ] Two **Discord** servers playing at once: the difference from one server is the per-stream
  cost without LiveKit mixed in.
- [ ] Several Fluxer servers playing at once: confirms LiveKit's part is only paid once.
- [ ] Memory a while after music stopped: confirms it goes back down.
- [ ] Lavalink's memory before it's removed, to complete the comparison with Red.
