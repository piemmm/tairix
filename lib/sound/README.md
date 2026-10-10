# tairix-sound

Stability tier: **experimental**.

First-party TAIRiX sound-file decoding: complete, fail-closed AU, WAV and
FLAC decoders (FLAC native and in Ogg) that turn an untrusted file into
interleaved PCM a block at a time, or a typed refusal — never a panic, and
never more memory than the caller's limits allow. `no_std` + `alloc`,
`forbid(unsafe_code)`. Behind the off-by-default `encode` feature, the one
encoder: FLAC, for the tests, the fuzz generator and the asset build.

## Consumers

Every player decodes through the parser sandbox: `lib/sandbox`'s
`audiodecode` worker links this crate, and the player never parses a byte of
a sound file in its own process (`plans/SOUND.md`). Moving and mixing samples
is `lib/audio`'s; this crate knows files and nothing of devices.

## Shape

`PcmSource::open` sniffs the format and reads its header; `next_block`
writes whole interleaved frames in the stream's `SoundInfo::sample` format;
`seek` enters a stream that can be entered; `probe` reads the header alone.
A file is read through `SoundInput` at offsets of the decoder's choosing, so
a four-hour recording decodes in the memory of one block. An input whose
bytes are not at hand answers `InputError::Unavailable`, and the call that
met it changes nothing, so its caller supplies the bytes and asks again.

`DecodeLimits` bounds what a file can make a decoder keep: channels, the
bytes its kept tags occupy (text and a fixed charge for each entry), and
cues and loops. Past a limit, metadata is left out and `Metadata::omitted`
says so; `Metadata::within` is the same rule, for a caller that must check
metadata decoded elsewhere. `Metadata::cover` is where a native FLAC file
holds its cover picture, for the caller to read and decode itself.

See `docs/src/lib/sound.md` for the formats, the refusals and the tests.
