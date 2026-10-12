# tairix-edid

What a display sink states about itself (`plans/SOUND.md` SND19j). Stability
tier: `experimental`.

- `Edid::parse` reads an EDID — the base block and its CTA-861 and
  `DisplayID` extensions — into the sink's identity and name, the distinct
  timings it takes with the one it prefers, its audio descriptors and
  speakers, and its HDMI and HDMI Forum capabilities; `block_count` says how
  many blocks to read first, the HDMI Forum override included.
- `Eld::parse` reads the same audio, and the monitor's name and link, from an
  HD Audio codec's ELD.
- `video_code` and `hdmi_video_code` name a timing by the code a source sends
  it under; `Avi` and `hdmi_vendor` pack the `InfoFrames` that carry it.
- `allocate` places a channel layout on the link's eight slots by its
  CTA-861 speaker allocation, `Allocation::fits` says whether a sink has the
  speakers it plays on, and `widest_layout` is the widest layout a sink
  places; `AudioInfo` packs the audio `InfoFrame` naming it.
- `synth`, behind the `test-support` feature, builds the synthetic
  descriptions the crate's tests and a display driver's read.

Every byte is untrusted device input, read by the least-authority process
that acts on it: each block's checksum is checked, every length and offset is
held to its block, the base block is refused when malformed and an extension
skipped, and what a sink lists is kept up to fixed bounds — sixteen blocks,
128 timings, 32 audio descriptors. A fuzz harness (`tests/fuzz_edid.rs`)
drives every reader.

The timings EDID names by reference — VESA DMT's, the established timings and
CTA-861's video codes — are the standards' own tables, checked against
themselves by the crate's tests. A standard timing DMT does not list is not
synthesised by CVT or GTF: the sink's own detailed timings state what it takes.

Every display driver and the HD Audio driver read through this one parser.
