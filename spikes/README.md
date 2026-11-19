# Phase 0 spikes

Throwaway experiments that answer the open questions in docs/handover.md section 4. They are not
workspace members; keep them out of the main build.

| Question | How to answer | Result |
|---|---|---|
| Can a bot account join Fluxer voice (LiveKit)? | Run fluxer-rust's own voice example against a test server (below). `!join <voice channel id>`, then `!play`. | _not yet tested_ |
| Does a self-hosted Fluxer instance work with a bot? | Set `FLUXER_API_URL` and run botsu; `$ping`. | _not yet tested_ |
| Slash commands on Fluxer? | fluxer-rust 0.2.1 has no interactions API. Check Fluxer's API docs for `/applications/{id}/commands`. | prefix-only for now |
| Discord voice with DAVE (songbird) | Only when the Discord adapter starts. | deferred |
| Hosting cost of self-hosted Fluxer + LiveKit (RAM, UDP ports) | Measure with `docker stats` once it runs on Unraid. | _not yet measured_ |

## Fluxer voice spike

Copy fluxer-rust's source (it ships its voice example and a test mp3) and run the example:

```sh
cp -r ~/.cargo/registry/src/*/fluxer-rust-0.2.1 spikes/fluxer-voice
cd spikes/fluxer-voice
FLUXER_TOKEN=... cargo run --example voice
```

For a self-hosted instance, uncomment the `.api_url(...)` line in `examples/voice.rs`.
ffmpeg must be on `PATH`. `spikes/fluxer-voice/` is a copy of a third-party crate; don't commit it.

Record results in the table above and in CLAUDE.md.
