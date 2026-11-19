# Phase 0 spikes

Throwaway experiments that answer the open questions in docs/handover.md section 4 and
docs/roadmap.md phase 0. Each is its own Cargo project (excluded from the workspace) with its own
`target/`. Record results in the table below and in CLAUDE.md.

| Question | How | Result |
|---|---|---|
| Can a bot play audio in a **Fluxer** voice channel (LiveKit)? | `fluxer-voice/` (below) | _not yet run_ |
| Can a bot play audio in **Discord** voice with DAVE encryption (songbird 0.6.1)? | `discord-voice/` (below) | _not yet run_ |
| Does Fluxer tell bots who is in which voice channel? | `fluxer-voice`: `!where`, and the log lines `GUILD_SYNC …` / `PASSIVE_UPDATES …` / `Voice state: …` | _not yet run_ |
| Fluxer moderation APIs (bulk delete, bans, timeouts, roles) | Tested live during phase 4 | folded into phase 4 |
| arm64 image possible? | webrtc-sys-build maps `aarch64` → prebuilt `linux-arm64` libwebrtc | yes, needs a native arm64 runner; not needed for Unraid |
| Self-hosted Fluxer RAM, ports | `docker stats --no-stream` on Unraid | _not yet measured_ |

## Running the voice spikes

Both test bots log in with the tokens from the repo's `.env` and use the prefix `!`.

1. **Stop botsu on Unraid** (Docker tab → botsu → Stop). The test bots use the same bot accounts;
   two connections could clash, and botsu would also see the `!` commands.
2. **ffmpeg**, once: `winget install Gyan.FFmpeg`, then open a new terminal. The spikes use it to
   generate `spikes/test-tone.mp3` (10 s, 440 Hz) on first run; Fluxer playback also needs it.
3. Join a voice channel yourself, then run one spike at a time:

   ```powershell
   cd spikes\fluxer-voice     # or spikes\discord-voice
   cargo run
   ```

4. In a text channel of the same server:

   | Command | |
   |---|---|
   | `!join` | joins **your** voice channel (`!join <channel id>` to pick one) |
   | `!play` | plays the test tone (`!play <file>` for another file; on Fluxer also a URL) |
   | `!stop` / `!leave` | stop playback / leave the channel |
   | `!where` | who the bot can see in voice, for troubleshooting `!join` |

5. Ctrl+C to stop the spike. **Start botsu on Unraid again.**

**Pass:** the bot appears in your voice channel and you hear the tone clearly for 10 seconds.
Copy the terminal output into the table above either way; on failure it says where it broke
(join timeout, LiveKit connection, DAVE handshake, ffmpeg).

More detail for Discord voice: `$env:RUST_LOG="info,songbird=debug"; cargo run`.
If `!join` says it can't see you, leave and rejoin the voice channel (that sends a fresh voice
state), then try again; on Fluxer, note whether `!where` was empty before that.
