# Phase 0 checks

The open questions from docs/handover.md section 4 and docs/roadmap.md phase 0. Record results
in the table below and in CLAUDE.md.

| Question | How | Result |
|---|---|---|
| Can a bot play audio in a **Fluxer** voice channel (LiveKit)? | `!music` on Fluxer (below) | **yes** (2026-11-19): joined and played the test tone |
| Can a bot play audio in **Discord** voice with DAVE encryption (songbird 0.6.1)? | `!music` on Discord (below) | **yes** (2026-11-19), after enabling symphonia's PCM decoder (songbird ships without codecs) |
| Does each platform tell bots who is in which voice channel? | `!music where` and `!music join` without a channel | **yes**: `!music join` finds the caller on both |
| Fluxer moderation APIs (bulk delete, bans, timeouts, roles) | docs read 2026-11-20; `api_spike` example against the instance ([below](#fluxer-api-spike)) | botsu's calls match the docs; live run pending |
| arm64 image possible? | webrtc-sys-build maps `aarch64` → prebuilt `linux-arm64` libwebrtc | yes, needs a native arm64 runner; not needed for Unraid |
| Self-hosted Fluxer RAM, ports | `docker stats --no-stream` on Unraid | ~1 GB RAM (2026-11-19); LiveKit ports 7881 and 7882 forwarded |

## Voice test

The voice test is built into botsu (`!music`, bot owners only), so it runs on the deployed
container; nothing to install locally. The image includes ffmpeg.

1. Update the container (Docker tab → Check for Updates → Apply Update).
2. Join a voice channel yourself. In a text channel of the same server:

   | Command | |
   |---|---|
   | `!music join` | joins **your** voice channel (`!music join <channel id>` to pick one) |
   | `!music test` | plays a 10-second 440 Hz test tone (`!music test <url>` for a direct audio link) |
   | `!music stop` / `!music leave` | stop / leave (it also leaves by itself after 10 minutes without sound) |
   | `!music where` | who the bot can see in voice; empty means voice states aren't arriving |

3. Do it on Discord and on Fluxer.

**Pass:** the bot appears in your voice channel and you hear the tone clearly for 10 seconds.

If something fails, the reply says where (joining, playing), and the container log has the
details: **Docker** tab → botsu → **Logs**, or `docker logs botsu 2>&1 | tail -50`. For more
detail, set the template's **Log level** field to `info,botsu=debug,songbird=debug` and Apply.

If `!music join` can't see you, leave and rejoin the voice channel (that sends a fresh voice
state) and try again. On Fluxer, note whether `!music where` was empty before that: it tells us
whether Fluxer sends a bot the "who's already in voice" snapshot.

## Fluxer API spike

### What the docs say (docs.fluxer.com/http-api, read 2026-11-20)

| Call | botsu sends | Docs | Notes |
|---|---|---|---|
| Bulk delete | `POST /channels/{id}/messages/bulk-delete`, `message_ids` (fluxer-rust) | 1 to 100 IDs, **any age**; unknown IDs skipped; 10 per 10 s | botsu applies Discord's 14-day stop on both platforms; Fluxer doesn't need it |
| Ban | `PUT /guilds/{g}/bans/{u}`, `reason`, `delete_message_days` (0-7), `X-Audit-Log-Reason` | also `delete_message_seconds` and `ban_duration_seconds` (0 or 60 s to 2 years) | Fluxer can expire bans itself; botsu's own scheduler works anyway |
| Unban | `DELETE /guilds/{g}/bans/{u}` | 400 `USER_IS_NOT_BANNED` if not banned | |
| List bans | `GET /guilds/{g}/bans` | bare array, not paginated; `user`, `reason`, `moderator_id`, `banned_at`, `expires_at` | |
| Kick | `DELETE /guilds/{g}/members/{u}` | as sent | |
| Timeout | `PATCH /guilds/{g}/members/{u}`, `communication_disabled_until` (RFC 3339 or null), `timeout_reason` | **up to 365 days**; null or a past time clears | botsu caps at 28 days (Discord's limit) |
| Add/remove role | `PUT`/`DELETE /guilds/{g}/members/{u}/roles/{r}` | removing a role the member lacks still returns 204 | |
| Roles | `GET /guilds/{g}/roles` | `permissions` is a decimal string; larger `position` ranks higher; @everyone = guild ID, position 0 | matches botsu's parsing |
| Audit reason | `X-Audit-Log-Reason`, percent-encoded, botsu truncates to 512 | percent-decoded, max 512, longer ones dropped silently | `reason` / `timeout_reason` in the body over 512 → 400, botsu's 512 cap covers it |

### Live check

`crates/botsu-fluxer/examples/api_spike.rs` runs botsu's own `FluxerChat` calls (plus a few raw ones
for limits) against the instance and prints PASS/FAIL per check. Needs a test server, a throwaway
text channel and a second account (not an admin). For the role, pass `new` and the spike makes
a temporary one (or give an existing role below the bot's highest role):

```powershell
./scripts/run.ps1 -Example api_spike -- <guild> <channel> <target user> <role> [--old <message id>] [--kick] [--ban]
```

Always run: bulk delete (2 fresh messages, then 1, 0, 101 and 100 unknown IDs), add/remove role,
2-minute and 30-day timeouts (both cleared again), the ban list, the bot's audit-log entries
(checks a reason with `é`, `%` and `✓` survives). `--old` bulk-deletes that message for real (pick
one older than 14 days) to confirm there's no age limit. `--kick` kicks the target (they need a new
invite). `--ban` bans, lists, unbans, unbans again (should fail), then tries a 1-hour ban via
`ban_duration_seconds` and lifts it.

Results. First run 2026-11-20 on the self-hosted instance, test server "Testing stuff", with the
bot as its own target (`self new`), so the account checks are still open:

| Check | Result |
|---|---|
| Bulk delete (botsu's call, 1 ID, limits) | ✅ botsu's call; **1 ID accepted** (Discord needs 2+); 0 and 101 IDs refused; 100 unknown IDs accepted |
| Messages older than 14 days | open: needs a message older than 14 days |
| Roles (create, give, take, take again, delete) | ✅ |
| Embeds (alerts) | ✅ kept with title and large image |
| File upload, download without a token (export/import) | ✅ |
| Channel list, server name | ✅ |
| Ban list | ✅ |
| Timeouts (2 min, 30 days, clearing) | open: needs a second account |
| Kick | open: needs a second account |
| Ban, unban, temporary ban | open: needs a second account |
| Audit-log reason intact | open: only timeout/kick/ban send one |

Finding: the bot's own role and a freshly created role both reported **position 1**. Fluxer
positions may not be unique like Discord's, so botsu's position comparisons (moderation
hierarchy, the addrole refusal hint) can see a tie. Check against a real role setup.

If the old-message and 30-day checks pass, botsu can drop the 14-day cleanup stop and allow longer
timeouts on Fluxer (per-platform limits in `Capabilities`).

