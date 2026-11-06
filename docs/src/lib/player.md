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

## Chained files

A chained file's links play in turn as one entry: when the decoder's link
ends it asks for the next, which follows the last into the stream when it has
its shape and waits for the stream to play out when it has another. Each link
is reported as it opens (`Note::Opened` with its link), with what it says of
itself (`take_link_metadata`), and `Status::link` names the link heard, whose
frames the position counts from. An entry's extent is measured over its whole
file: each link carries how far into the entry it begins, so a start lying in
a later link is found there and a duration runs on across links. A seek moves
within the link heard — reopening the file at that link where the decoder has
moved on — and past its end goes on into the next.

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
loudness — may change in between. That holds for an engine begun with nothing
to play and for one whose last file the decoder refused: a jump waits only for
the decoder to take a request (`AudioDecodeClient::takes_request`), which one
holding no file does as readily as one idle with a file open. A device change
mid-playback reopens the stream on the frame the old one stopped on.

## Reading a track's tags

`probe::TagReader` reads a file's stream and tags through the same sandboxed
decoder, as a state machine shaped like the engine: `begin` a file, `pump`
after every wake, park on `members` no later than `deadline`, and hand what
fired to `on_ready`. A pump queues every request to the worker
(`supervise::send_waiting`), takes every answer and serves every byte asked
for before it returns, so it never parks holding a request its session was not
given. A file gets `probe::BUDGET_NS`, ten seconds, from when its open reaches
a live worker, so a replacement the restart pacing holds back costs it time
and never its tags; a worker that cannot be launched leaves it `Tags::Unread`.
A worker that overruns the budget, fails, or is abandoned part-way through a
request — by `abandon`, or by a `begin` over a reading under way — is killed
and the next file read by a replacement, since one left mid-request would
answer that file with this one's frames. A refusal, or bytes that cannot be
read, leave the file `Tags::Unread` and the worker serving.

## The live seams (`rt`)

`rt::live` builds the engine over the runtime: `RtFiles` opens paths,
`HeldFiles` reads descriptors the program shares with it (so a removed entry's
descriptor closes only once the engine is done with it), `RtSpeaker` is the
audio service's stream, and `EngineWaits` keeps a host's wait-set watching the
decoder's pipes and the stream's mailbox. `RtTags` reads a track's tags to the
end on the thread that asks — file I/O and a wait, so a worker's and never an
interactive loop's — parked on a wait-set of its own.

## Testing

The engine is host-tested over the real decoder in-process, against a fake
audio service that plays a period at a time: exactness, gapless files, passes,
seeks, pauses, the seat's hold, edits on every path (ahead of the decoder,
waiting behind a full ring, already in the stream, being heard), a device
change, a new playback after one ended, Replay Gain applied bit for bit against
its own computation, a run of 100 000 entries that cannot be opened walked in
one loop, two rapid skips, and chained files — links of one shape gapless,
a link of another shape on a stream of its own, an extent across links, and a
seek past a link's end. A playback begun with nothing plays the file it is then
given and jumped to, with the decoder's pipes reported only as a host's
wait-set reports them (`tairix_sandbox::loopback::PipeSessionLauncher`), and a
jump after a refused last file plays. The tag reader runs over the real
decoder behind that same pipe model: tags read under its readiness, a file
larger than the pipe, a hung worker killed at the budget and replaced, a
replacement held back past the budget still reading the next file, a worker
that cannot be launched leaving its file unread, a reading abandoned part-way
killing its worker, a refusal and an unreadable file leaving the worker
serving, and no seed reading nothing.
