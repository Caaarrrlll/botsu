# Phase 0 checks

The open questions from docs/handover.md section 4 and docs/roadmap.md phase 0. Record results
in the table below and in CLAUDE.md.

| Question | How | Result |
|---|---|---|
| Can a bot play audio in a **Fluxer** voice channel (LiveKit)? | `!voice` on Fluxer (below) | _not yet run_ |
| Can a bot play audio in **Discord** voice with DAVE encryption (songbird 0.6.1)? | `!voice` on Discord (below) | _not yet run_ |
| Does each platform tell bots who is in which voice channel? | `!voice where` and `!voice join` without a channel | _not yet run_ |
| Fluxer moderation APIs (bulk delete, bans, timeouts, roles) | Tested live during phase 4 | folded into phase 4 |
| arm64 image possible? | webrtc-sys-build maps `aarch64` → prebuilt `linux-arm64` libwebrtc | yes, needs a native arm64 runner; not needed for Unraid |
| Self-hosted Fluxer RAM, ports | `docker stats --no-stream` on Unraid | _not yet measured_ |

## Voice test

The voice test is built into botsu (`!voice`, bot owners only), so it runs on the deployed
container; nothing to install locally. The image includes ffmpeg.

1. Update the container (Docker tab → Check for Updates → Apply Update).
2. Join a voice channel yourself. In a text channel of the same server:

   | Command | |
   |---|---|
   | `!voice join` | joins **your** voice channel (`!voice join <channel id>` to pick one) |
   | `!voice play` | plays a 10-second 440 Hz test tone (`!voice play <url>` for a direct audio link) |
   | `!voice stop` / `!voice leave` | stop / leave |
   | `!voice where` | who the bot can see in voice; empty means voice states aren't arriving |

3. Do it on Discord and on Fluxer.

**Pass:** the bot appears in your voice channel and you hear the tone clearly for 10 seconds.

If something fails, the reply says where (joining, playing), and the container log has the
details: **Docker** tab → botsu → **Logs**, or `docker logs botsu 2>&1 | tail -50`. For more
detail, set the template's **Log level** field to `info,botsu=debug,songbird=debug` and Apply.

If `!voice join` can't see you, leave and rejoin the voice channel (that sends a fresh voice
state) and try again. On Fluxer, note whether `!voice where` was empty before that: it tells us
whether Fluxer sends a bot the "who's already in voice" snapshot.
