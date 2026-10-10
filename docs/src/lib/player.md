# `tairix-player`

The playback engine both players run: `play` from a command line and a
terminal, `music.app` from a window (`plans/SOUND.md` SND10, SND14). A
programme of sound files is decoded in the parser sandbox and written into one
`audio-v1` stream; the engine is a state machine its host feeds what it is
woken by, so neither player keeps playback on the loop that owes its user a
frame.

## The programme

The engine names an entry only by its `EntryId`, which survives the programme
being reordered or edited beneath it, and asks the `Programme` what follows each
entry as it reaches it: `next` within a pass, `previous`, and `first_of_pass`
for whether another pass begins. An entry names an `Item` the engine's `Files`
seam opens — a path for `play`'s fixed `List`, a descriptor the program holds
for `music.app`'s playlist — so the engine never learns which.

An edit goes through `Engine::edit`. The entry being heard plays on; whatever is
queued after it that no longer follows from the programme is dropped — work not
yet in the stream silently, frames already in it by stopping on the frame being
heard and refilling from there — so a removed or moved entry is never heard out
of place. An entry being heard that the edit removes is left at once for what
followed it: until the engine settles (`Programme::settle`), a removed entry
still answers `next` with its successor.

## One stream while the shape holds

Consecutive entries of one rate, sample format and channel layout are written
into the same stream back to back, so a programme plays gapless; an entry of
another shape waits for the stream to drain and opens its own. The ring counts
frames from zero for each stream and a file counts its own; segments record
which file frame each run of ring frames carries, so the position the service
reports reading is turned back into the entry and frame being heard.

A seek or a skip works from where the listener is: a waiting jump's target,
else the stream's own front segment — never a position the service reported
before the command, so two presses of *next* move two entries on.

## Loudness

With `Settings::normalise` each track plays at the Replay Gain track gain its
own tags state (`loudness::track_millibel`), capped by the headroom its stated
peak leaves so it cannot clip; a track stating no peak is never raised. The gain
is applied to the decoded frames before they reach the ring and dithered back
onto their grid, so a gapless boundary between two tracks of different gains is
exact; a track at unity is passed through untouched, which keeps the bit-exact
path bit-exact.

## After playback ends

An ended playback leaves the engine usable: a `Control::Jump` begins another,
restarting the decoder if it is not live, and the settings — level, device,
loudness — may change in between. A device change mid-playback reopens the
stream on the frame the old one stopped on.

## The live seams (`rt`)

`rt::live` builds the engine over the runtime: `RtFiles` opens paths,
`HeldFiles` reads descriptors the program shares with it (so a removed entry's
descriptor closes only once the engine is done with it), `RtSpeaker` is the
audio service's stream, and `EngineWaits` keeps a host's wait-set watching the
decoder's pipes and the stream's mailbox.

## Testing

The engine is host-tested over the real decoder in-process, against a fake
audio service that plays a period at a time: exactness, gapless files, passes,
seeks, pauses, the seat's hold, edits on every path (ahead of the decoder,
waiting behind a full ring, already in the stream, being heard), a device
change, a new playback after one ended, Replay Gain applied bit for bit against
its own computation, a run of 100 000 entries that cannot be opened walked in
one loop, and two rapid skips.
