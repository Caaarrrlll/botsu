# Vendored crates

## fluxer-rust 0.2.1

A copy of [fluxer-rust 0.2.1](https://crates.io/crates/fluxer-rust) (MIT, see its LICENSE), used via
`[patch.crates-io]` in the workspace `Cargo.toml`. The `audio/` sample folder is left out.

botsu's changes (search for "botsu patch"):

- `src/model/mod.rs`: `Guild` keeps `voice_states` from GUILD_CREATE. Upstream drops them, so after
  a restart the bot couldn't see who was already in a voice channel (`!music join`/`play` failed
  until people rejoined).

Drop the copy when an upstream release has these, and re-check the notes in CLAUDE.md first.

- `src/voice/mod.rs`: `FluxerVoiceConnection::play_input(input, stdin)`. Upstream's `play_music`
  hardcodes ffmpeg's arguments (no reconnect flags), can't read from another process, never drains
  ffmpeg's stderr while playing, and posts errors to a text channel. botsu pipes `yt-dlp -o -`
  into it and gets errors back from the task.
