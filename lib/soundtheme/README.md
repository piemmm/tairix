# tairix-soundtheme

Stability tier: **experimental**.

The desktop's sounds, as policy: which asset each `SoundEvent` plays
(`event`), the shipped themes under `/System/Audio/Sounds/<Theme>/` and the
bounds a theme is read within (`catalog`), the user's sound settings
(`settings`), and the client a program asks the cue authority with (`cue`).
The settings, the user's layer and their resolution are the default `settings`
feature; a program that only cues turns it off. `rt` adds the live transport.

It knows nothing about decoding (`lib/sound`) or mixing (`lib/audio`). The
event vocabulary itself is wire vocabulary, so it lives in `lib/abi` with the
cue protocol and is re-exported here.

The shipped theme is `assets/TAIRiX/`: one 48 kHz stereo 16-bit FLAC per
asset, encoded by `cargo xtask sound-encode` and checked at build time by
`tools/syshelp`. The design is `plans/SOUND.md` §Default desktop sounds; the
reference is `docs/src/lib/soundtheme.md`.
