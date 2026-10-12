# `tairix-edid`

`lib/edid` reads what a display sink states about itself: the EDID a display
driver reads over the sink's DDC bus, and the ELD an HD Audio codec keeps for
the display on its pin (`plans/SOUND.md` SND19j). One parser serves every
display driver and the HD Audio driver.

## The EDID

`block_count` states how many blocks to read: the base block's extension
count, or the count an HDMI Forum override (EEODB) at the head of the first
CTA extension states, at most sixteen. `Edid::parse` reads them:

- **The sink.** Its manufacturer, product code, serial and date; whether its
  input is digital; its name; the picture's size, from the preferred timing
  where it states one; the fastest pixel clock its range limits allow.
- **Timings.** Each detailed timing, the established timings, the standard
  timings DMT lists, the established timings III, each CTA-861 video code and
  HDMI 1.4 video code it lists, and each `DisplayID` type I and type VII
  timing — merged so each timing appears once, carrying the code the sink
  listed it by and whether the sink prefers it. From revision 4 the first
  detailed timing is the preferred one; before, a feature bit says so. An
  interlaced detailed timing's field figures are doubled into the frame's.
- **Audio.** The short audio descriptors — the coding, the most channels, the
  rates, and for linear PCM the sample sizes — the speaker allocation, and
  the basic-audio flag.
- **HDMI.** Whether the sink is an HDMI sink at all, its CEC physical address
  and fastest TMDS clock, and from an HDMI Forum block (vendor-specific or
  the HDMI 2.1 extended one) its fastest TMDS character rate above 340 MHz,
  whether it has SCDC and requests reads over it, and whether it descrambles
  below 340 MHz too; and from the video capability block whether a source may
  state an RGB quantization range.

## Naming a timing, and the InfoFrames

A source names the timing it sends by a code whatever list the sink gave it
in. `video_code` finds a timing's CTA-861 code, a 59.94 Hz variant's
included, the lower of two that differ only in aspect; `hdmi_video_code`
finds the HDMI 1.4 code of one of the four 4K timings, which a source sends
to any HDMI sink in its vendor-specific `InfoFrame`, as Linux's
`drm_mode_hdmi_vic` has it, since a sink older than CTA-861-F knows no CTA
code for them.

`Avi::pack` packs the AVI `InfoFrame` — the CTA code and its picture aspect,
the active format the picture's, the RGB range — at version 3 for a code past
version 2's seven bits; beside an HDMI code it carries that timing's aspect
and no code. `hdmi_vendor` packs the vendor-specific frame naming an HDMI
code, and `AudioInfo::pack` the audio frame naming a linear PCM stream's
channel count and speaker allocation, the rate and sample size left to the
samples' own channel status as CTA-861 asks. Each is laid out as a
transmitter's packet memory takes it: the header, the checksum that sums the
frame to zero, then the payload.

## Placing a stream's channels

An HDMI audio sample carries eight slots, and CTA-861's speaker allocations
(CTA-861-H Table 34) say which speaker each plays on. `allocate` places a
channel layout — the allocation code the audio `InfoFrame` states, and the
layout channel each slot carries — only where its channels fill the first
slots with none left empty: stereo, 2.1, 3.1 with or without a rear centre,
5.1, 6.1 and 7.1, a layout with both side and rear pairs placed as CTA-861
places back-surround, and a rear centre in the rear pair's first slot without
that pair and behind it with one, as Table 34 places it. `Allocation::fits`
says whether a sink has every speaker an allocation plays on, the front pair
taken as given, and `widest_layout` is the widest conventional layout of a
channel count that is placed and fits: 7.1, 5.1 or stereo. A sink's rates are
a `SampleRates` set, `SampleRates::BASIC` the three basic audio guarantees.

## Fail-closed

Every byte is untrusted device input. The base block must carry the EDID 1
header and a good checksum or the whole is refused; an extension failing its
checksum, a `DisplayID` section failing its own, or a block of a kind not read
is skipped. Every data block, descriptor and timing is held to the block it
lies in: a data block running past its collection ends it, a timing whose
porches outrun its blanking is dropped, a reserved video code or audio coding
is skipped. What a sink lists is kept up to fixed bounds — 128 distinct
timings, 32 audio descriptors — the first of each kept. A name must be
printable ASCII or there is none.

## The ELD

`Eld::parse` reads an ELD of version 2: the baseline block held to the bytes
read, the monitor name held to sixteen characters and to the baseline, the
audio descriptors held to the baseline, the speaker allocation, and the link —
HDMI or `DisplayPort` — refusing a connection type no link has.

## The tables

The timings EDID names by reference are the standards' own: VESA DMT's 88,
the 17 established timings and 44 established timings III, CTA-861's 154 video
codes and HDMI's 4. The crate's tests check them against themselves — every
DMT timing refreshes at its stated rate, every established timing III is a
DMT timing, the codes ascend without a gap. A standard timing DMT does not
list is not synthesised by CVT or GTF.

## Testing

The behaviour tests read synthetic descriptions laid out field by field —
monitors, televisions with every data block, the HDMI Forum override,
`DisplayID` sections, ELDs — and the fuzz harness (`fuzz_edid`) mutates them,
half with their checksums repaired so the mutations reach the decoding behind
each checksum. The builders are the crate's `synth` module, behind the
`test-support` feature, so a display driver's tests build their sinks from the
same descriptions; it is never in a TAIRiX build.

## Stability

Tier: `experimental` (see the crate `README.md`).
