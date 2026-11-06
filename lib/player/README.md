# tairix-player

Stability tier: **experimental**.

The playback engine: a programme of sound files decoded in the parser sandbox
and written into one `audio-v1` stream, gapless while consecutive files share a
shape. `no_std` + `alloc`, `forbid(unsafe_code)`.

## Consumers

Both players: `play` (the command, `userland/apps/play`) and `music.app` (the
desktop player, `userland/apps/music`). Neither parses a byte of a sound file in
its own process; the engine's decoder is a `lib/sandbox` `audiodecode` worker.

## Shape

`Engine` is a state machine its host feeds what it was woken by — decoder
traffic, the stream's notifications, a deadline, a `Control` — and reads
`Status` back from. What plays is a `Programme`, which names entries by a stable
`EntryId` and says what follows each; `List` is the fixed one `play` runs.
`Engine::edit` changes the programme and re-plans only what is still to be
heard. `Settings` carries the level, the device and whether a track's own
Replay Gain is applied.

`TagReader` (`probe`) reads a track's stream and tags through the same
decoder: a state machine shaped like the engine, which never parks holding a
request its session was not given, gives a file ten seconds once a decoder
takes it, and kills a worker it abandons part-way through a request.

The `rt` feature adds the live seams: files opened by path (`RtFiles`) or held
as descriptors (`HeldFiles`), the audio service's stream (`RtSpeaker`), the
wait-set members a host parks on (`EngineWaits`), and the tag reader a worker
thread drives to the end of each file (`RtTags`). Everything else performs no
I/O and is host-tested.

See `docs/src/lib/player.md` for the design and the tests.
