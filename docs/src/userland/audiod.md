# Audio service (`audiod`)

`userland/system/audiod` is the one mixer, router and audio authority
(`plans/SOUND.md` SND4). It is the **sole holder of `CAP_AUDIO_DEVICE`** and
the only process that speaks `audiochan-v1`; every program plays or records
through `audio-v1`, and there is no second path.

One system service, not one per user. The device is machine state, so the
arbiter is a machine service with per-seat routing and per-principal
accounting — a per-user daemon is precisely the design that cannot arbitrate
between two logged-in users over one piece of hardware.

## The path a sample takes

```
program ──audio-v1──▶ audiod ──audiochan-v1──▶ driver process ──▶ hardware
        client ring              device ring
```

A client creates its own PCM ring and `shm_grant`s it inward; `audiod` creates
the *device* ring and grants it outward to the driver. The two directions are
spelled separately in the service's region seam, so a handle of unstated
provenance can never be mistaken for either.

Frames move on the device's own period interrupt and nothing else. The driver
services its ring from that interrupt and sends one notification carrying the
`(frame position, monotonic time)` pair; `audiod` folds that pair into the
endpoint's clock model and refills. There is no audio tick anywhere in the
system, and nothing polls.

## What it composes

Every decision about *what samples come out* is `lib/audio`'s:

| Stage | Where |
|---|---|
| Which sink a stream lands on | `route::route` |
| What the seat's room does with a stream | `route::admit`, over `route::Room` |
| The ducking rule | `route::duck_millibel` |
| Source layout onto sink layout | `ChannelMatrix::derive`, once at open |
| The one rate conversion | `Resampler` over a `FilterBank` shared per rate pair |
| Summing and the one quantisation | `Mixer` |
| Four gains into one multiply | `volume::resolve` |
| What a device's rate actually is | `ClockModel` |

## Real-time discipline

Every buffer is allocated at stream-open or device-configure and reused. The
per-period path allocates nothing: it reads each client ring into that
stream's own scratch (so exactly one shared region is borrowed at a time) and
folds the live streams into the mixer as an *iterator* rather than building a
per-period collection. Its memory is pinned so an audio buffer never reaches
swap, and its mixing path runs at real-time priority; a machine that refuses
either is told so on the log and served anyway.

## Accounting

A running stream with nothing queued contributes silence for exactly the
frames it missed and takes the under-run — the device would otherwise run dry,
which is strictly worse and is what its driver would report instead. A
*draining* stream is different: the pump emits only the frames it genuinely
has, so a drain's tail is a short chunk rather than a padded one. The position
never lies, so a client resynchronises exactly rather than drifting.

A client that corrupts its own ring — positions no producer could publish —
faults that stream alone: it is told `Faulted`, it moves no frame again, the
stream and its owner are recorded (`STREAM_FAULTED`), and every other
principal's sound, and the device, carry on. A *device* ring is the driver's
and the mixer's, so a fault there is the device's, and loses it.

## An endpoint's configuration

The first stream opened on an endpoint configures it, and the configuration
stays while any stream is open on it. When nothing on it is live — every
stream paused, drained, idle or held — its clock is stopped, so the device
takes no interrupts. A sink first plays out what the device already holds:
those frames were mixed before the last stream stopped, so a resume is exact,
and held in the device they would play into whatever starts next. A source
stops at once and drops what it captured past its streams' positions. A paused
stream resumes into the configuration it left, at the frame it stopped on, and
a stream reaching a stop it scheduled winds the endpoint down the same way.
The last stream's close hands the endpoint back to the driver. A stream that
goes live while the device is playing out a drain is clocked again when the
drain completes, rather than left on a stopped device.

A stop may be scheduled before the stream starts: a start keeps a stop that
lies ahead of it, so a client can name a segment's end before the device
moves.

## Following the seat

A seat's speakers and microphones serve the room its display lease describes.
`audiod` subscribes to the boot seat's `DisplayLease` notice — readable only by
the services owning the seat's devices — and on every edge recomputes the room
(`route::Room`) and re-decides every stream with `route::admit`:

| The seat | The room | Whose streams move |
|---|---|---|
| held by a process a login session encloses | `Session` | that session's |
| held by a presenter in a login session no stream is in (the login screen) | `Session` | nobody's |
| being handed between presenters | `Withheld` | nobody's |
| with its text console, or never held | `Unclaimed` | anybody's — the headless case |

Before the room changes, every clocking source hands what it has captured to
the streams the old room admitted. Then a stream outside the room is **held at
a frame boundary and told**
`SeatInactive` — a paused one too, so a paused player can say why playing on
would wait. On the switch back it resumes from the exact frame. A departing
session's music does not play into the arriving session's room, its recorder
does not hear it, and neither silently vanishes; what the device was already
handed plays out rather than waiting for the next room. A notification outside
the room is noise by the time the room is its own, so what it queued, and what
it writes while outside, is dropped — the jump in its position says exactly
which frames went — and one draining when the room left it is over. Opening
or starting a notification outside the room is refused `SeatNotOwner`, so a
client is never told a sound was accepted that will not be heard.

Until the lease is first read the room is nobody's (fail closed), and a lease
that cannot be read leaves it so, saying why (`NOTICE_UNAVAILABLE`). Each room
change is recorded (`ROOM_CHANGED`).

## The recording indicator

Whenever the number of capture streams moving frames changes — a capture
started, stopped, closed, held or released by the room, or lost with its
device — `audiod` publishes it as the `AudioCapture` notice, which only the
holder of its reserved rendezvous may publish and anyone may read. The session
draws its recording indicator from it, so no program can hide its own
recording.

## Device controls

`SetDefault`, `SetLevel` and `SetMute` are admitted for the room's tenant: the
login session holding a `Session` room, anybody in an `Unclaimed` one, nobody
while it is `Withheld` (`SeatNotOwner`). Each tenant's levels, mutes and
default preferences are kept against it, keyed by location (`controls.rs`), so
a returning session's controls are in force again before any of its held
streams resumes. A session's controls are forgotten once it neither holds the
room nor owns a stream; the unclaimed room's never are.

A level is programmed into the device's control where it has one and the
mixer applies the remainder; a control the driver refuses as unsupported
falls back to the mixer, and any other refusal loses the device. A direction's
default is the live endpoint the tenant prefers, else the one the machine's
baseline prefers, else the first bound, chosen again whenever a device is
bound, lost or reaped, the room moves, or a preference changes. The baseline —
the preferred sink and source and the level every endpoint starts at — is
delivered by the device manager from `system.conf`.

A lost device is never a default and is never listed, and is reaped once no
stream rides it; its slot is the next bind's. Each change to a device or its
controls moves a count the `AudioDevices` notice publishes, which the desktop
session follows to remember its user's choices.

## Authority

* **Playback needs no capability.** The authorisation is that the caller's
  login session holds the seat whose room the device serves, decided by the
  one routing policy from the kernel-attested origin — at open, and again for
  every live stream each time the lease moves. A capability every program
  would hold is not a boundary.
* **Capture demands `CAP_AUDIO_CAPTURE`**, read from the caller's attested
  capability summary at stream open and never from anything the caller said.
* **Adopting or retiring a driver's channel, and delivering the baseline,
  demand `CAP_DRV_LOAD`.** The authority to put a driver on the machine is
  exactly the authority to tell the mixer about one, so no third capability
  is minted: `devmgr` holds it, and an ordinary program cannot reach these
  operations at all.
* **Device controls need no capability**: the room's tenancy is the boundary,
  as it is for playback.
* **Listing every stream demands `CAP_SYSINFO_INTROSPECT`**, held by the
  System Information service, which scopes the streams to their owners.

A stream id is a service-issued token checked against the attested pid, so a
guessed id reaches nothing. Every capture grant **and refusal**, every device
bound or lost, every default-device change, every device control changed **or
refused**, and every baseline adopted lands on the hash-chained audit log with
a stable event id — "who tried" is the question an incident asks.

## Not yet

Every device is the boot seat's: no device is assigned to a seat of its own.
