# Cue authority (`soundd`)

`userland/system/soundd` makes the desktop's sounds (`plans/SOUND.md` SND21).
A program asks for one by its event over [`cue-v1`](../abi/sound-cue.md); the
service decides from the caller's kernel-attested identity whether it may,
resolves the event to a theme's asset or to silence, decodes the asset in the
parser sandbox, and plays it as a `Notification`-role stream through
[`audiod`](./audiod.md). Init starts it at boot under its own service account
(`soundd`, uid 21) and restarts it on failure.

It is a player, not part of the mixer: untrusted bytes never decode in the
process that holds every stream, and its file reads stay off the mixer's
real-time path. It is not the session either, because `Startup` precedes every
session, `Shutdown` outlives them, and a headless machine has none.

## Who may cue what

| Event class | Played for | Admitted when |
|---|---|---|
| Lifecycle (`Startup`, `Shutdown`, `Login`, `Logout`, `DeviceAttached`, `DeviceDetached`) | the machine (`Tenancy::Machine`) | the caller owns the transition (`SoundEvent::owned_by`); otherwise `PermissionDenied`, recorded as `CUE_REFUSED` |
| Application (`Notification`, `NotificationMessage`, `NotificationReminder`, `Error`, `Warning`, `Info`, `Bell`) | the caller's login session (`Tenancy::Session`), or none for a daemon | `audiod` admits the stream: only while that session holds the room, so its `SeatNotOwner` is the caller's answer |

No new capability: the check is the caller's attested pid, uid or capability
summary, and the room is `audiod`'s to judge.

## The user's layer

While a desktop session holds the seat, its user's own choices govern the
sounds heard there (`plans/SOUND.md` SND23). The session pushes them over
[`cue-v1`](../abi/sound-cue.md)'s `Policy`: a settings document naming no file,
and each chosen file beside it as a read-only delegation to this instance,
which it learns from the [`CueAuthority`](../abi/notice.md) notice this service
publishes once it has bound.

- **Only the session at the seat.** A push is admitted only from the anchor of
  the login session the seat's display lease names at that moment — the
  service follows the `DisplayLease` notice, which binding the cue rendezvous
  admits it to — and the holder is read again before every request. When the
  lease moves on, the layer is dropped, its files closed, and the decodes of
  them forgotten (`POLICY_DROPPED`). A push from anyone else is `SeatNotOwner`
  (`POLICY_REFUSED`); a document naming a file, or any setting of a sound no
  user's choices reach, is `OutOfRange` and refused whole. A refused push's
  delegations are redeemed and closed all the same, so none stays pending
  against the session.
- **Every sound heard at the seat but the machine's moments.** `Startup`,
  `Login` and `Logout` sound when no session holds the seat, so they are the
  machine document's alone; the layer governs every other sound heard at the
  seat — the machine's own, `Shutdown` and the device sounds among them, and
  the seat session's own applications' — and nothing another session cues,
  which the machine document decides, so no other session learns the choices
  or reaches the files. What the user left is the machine's: the layer is the
  pushed document laid over the machine document, laid again whenever that
  changes.
- **A user's file sounds only in their room.** A session's own file is that
  session's data, so a cue sounding one — even a machine's moment — plays under
  the session's tenancy, and the audio service stops it when the room moves on.
  A machine's moment still waiting on such a file when the seat moves sounds as
  the seat now decides instead, and a decode no cue waits on any more is
  dropped. The service reads the seat on every turn that decides something — a
  request, the decoder, the lease, a deadline — so a decode landing beside a
  lease move sounds for the seat's new holder; a stream's own wake decides
  nothing of the seat's and reads nothing.
- **A file is a descriptor.** A handed file is read under the session's own
  identity, the delegation being this service's whole authority over the
  user's files; this service never learns its path, and opens no user's file by
  name. One file chosen for several events is one delegation, redeemed once and
  shared between them.
- **A push again keeps what is still true.** The session pushes again on every
  change its user makes, handing every chosen file over afresh. A file handed
  over unchanged keeps its decode, or the record that it does not decode; a
  file changed since is read again, and the earlier layer's files close. A
  decode or a refusal that lands after its session left the seat is not kept,
  so nothing of the session outlives its tenure.

## Where the sounds are cued

Each site posts its cue and collects the answer from its own wait-set, so
nothing that makes a sound waits for one (`plans/SOUND.md` SND22).

| Sound | Cued by | When |
|---|---|---|
| `Startup` | init | once per boot, when every boot-floor service has settled (`boot-complete`) and the writable root is mounted (`filesystems-mounted`) |
| `Shutdown` | Switchboard, the `CAP_SYSTEM_POWER` holder | before it powers off or restarts, which waits for the sound to play out, bounded by its length and a second (`PLAYED_OUT_WAIT_NS`) |
| `Login` | the login service | once it starts a session: a text one, or a fresh graphical one, not one a switch resumes |
| `Logout` | the login service | once a session ends, not when a graphical one steps aside for a switch |
| `DeviceAttached`, `DeviceDetached` | the device manager | a removable device arriving on, or leaving, a running machine ([hardware detection](../drivers/hardware-detection.md)) |
| `Notification`, `Warning`, `Error` | the desktop session | a notification newly shown in the icon bar's area, by severity: information and success, a warning, a critical one; replacing one already shown is silent |
| `Bell` | `terminal.app` | the hosted program rings the bell (`^G`), once per batch of output |

A cue that cannot be posted is given up on at once; a cue site never retries.

## What a cue resolves to

`tairix_soundtheme::resolve`, in one order: nothing if the event or every sound
is off; the file chosen for the event — the session's own, else the
replacement the machine's document names, which this service reads itself; the
active theme's asset; the shipped theme's; otherwise silence. A chosen file
that cannot be read is `Undecodable`, never covered by the theme's. A cue that
makes no sound still succeeds and says why — `Disabled`, `NoAsset`,
`Undecodable`, `Coalesced`, `TooMany` or `NoDevice` — and every one is counted
(`Tally`). There is no synthesised fallback beep.

The machine's policy is its sound document, `/System/Settings/Sound/sound.conf`
(`MACHINE_SETTINGS`), read tolerantly through the same registry a user's is. It
is read again only when the file changes, and a document that cannot be read
leaves the last one read in force (`SETTINGS_REFUSED`).

## Decoding and the cache

Each sound is read whole, within `MAX_SOUND_BYTES`, and decoded in a supervised
session worker — the service's own binary in the sandbox's session role —
within `MAX_CUE_SAMPLES`, `MAX_CUE_SECONDS` and a five-second budget. Only a
length the stream declares is reserved up front; one of unknown length grows
as it decodes, within the bound. A chained Ogg file is read through its links,
the cue the links together within the bound; a link of another shape than the
first cannot be part of one sound, and refuses the cue. A worker that crashes,
breaks the protocol or overruns is replaced for the next sound rather than
resumed. A theme's asset is FLAC, as the build holds it; any other file — a
user's own, a replacement — is decoded as whichever format the worker finds it
in. A sound that would not decode — malformed, past a cue's bounds, or one that
crashed or overran its worker — is remembered, by version, so it is not decoded
again for every cue. A failure of the machine's own is not the sound's: one
the memory, the volume or a worker it could not key refused is tried again by
the next cue.

Decoded sounds are kept in a reclaimable cache (`lib/reclaim`, a mebibyte per
gibibyte of RAM), keyed by where the file came from — a theme's asset, the
machine's replacement for an event, or a session's own file for an event — and
the file's version, so one decode serves a boot's worth of cues and a replaced
file is a new sound. A user's own file is theirs, so the cache is private: an
entry is zeroed as it is given up, and a decode a sounding cue still holds is
zeroed when it lets it go. Nothing about a cue is pinned: losing one under
pressure costs a re-decode.

## Playing

A cue opens its stream on the default sink at the asset's own format and rate,
with a ring of the whole sound up to the ring bound; a longer sound is refilled
as the device frees room and drained once queued. A shipped sound is 48 kHz at
unity gain, so it reaches the device bit-exact; the settings' one gain is the
stream's. A stream that is told the room moved, the device went, or its ring
broke ends the cue; one that has not played out by its length plus a second is
stopped (`CUE_CUT`). What a cue's mailbox holds is always read before its
deadline is kept, so a cue that played out while the authority was busy is
retired as played, never cut; and a notification naming another stream — a
closed one whose mailbox this one inherited — ends nothing. A cue asked to be
answered once played out (`Answer::PlayedOut`) is answered then, or at that
deadline.

A repeat of a sound already sounding or decoding for the same room joins it
rather than sounding over itself: a burst of one event — nine disks attaching
at once — is one sound, heard once, and costs one of the four slots. A joiner
waiting for the sound to play out is answered when the one it joined does; one
waiting only for the start is answered at once, `Coalesced`. A machine sound
that played out whole is recorded (`MACHINE_CUE_PLAYED`, naming the `event`);
one that was cut or broke off is not.

## Bounds

| Bound | Value | Past it |
|---|---|---|
| Cues sounding or decoding at once | 4 | `TooMany`, dropped rather than queued |
| A repeat of one event by one principal | 200 ms | `Coalesced`, and it costs nothing |
| Callers waiting on one sound they joined | 16 | answered at once, `Coalesced` |
| A principal's cue rate | 8 at once, 4 a second | `LimitExceeded`, recorded once a flood (`CUE_LIMITED`) |
| Principals tracked | 64 | `LimitExceeded`; an idle record is reclaimed first |
| Calls the rendezvous holds | 16 | refused at post by the kernel |
| A pushed settings document | 2048 bytes | refused at decode |

Each is a containment bound, not a capacity: a desktop makes a handful of
sounds a minute.

## The serve loop

One wait-set: the reserved `SOUND_CUE_ENDPOINT`, the decode worker's pipes in
the directions it wants, each sounding cue's stream mailbox, the memory band,
and the seat's display lease. It parks until one of them, or the authority's
next deadline, wakes it, and whatever wakes it keeps the deadlines that have
come, so a source kept ready cannot hold a decode past its budget or a sound
past its end. A dead wait-set exits for init to restart rather than spinning. A lease that cannot be read is no session at the seat, so no choices
apply.

## Testing

The authority is pure over five seams — the store, the streams, the decode
launcher, the cache's gauge and the audit sink — with the seat's holder handed
in on every request, and is host-tested over the real in-process decoder: every
authority class, every path to silence, the bounds, the cache, refilling a
sound longer than its ring, a deadline, and the user's layer — who may push,
what a push may say, a handed file in another format playing and leaving with
its session, one file shared between events, a session's file sounding only in
its room and a waiting machine's moment sounding as the seat decides once it
moves, another session's cue kept from the choices, a push again keeping the
decodes and refusals of unchanged files, a late refusal not held against a
returning session, a busy rendezvous keeping its deadlines, the machine's
failures tried again, the holder read again before every request, the
machine's document changing beneath the choices, and the machine's replacement
files. The
`audio_qemu_*` cue verticals ask for `NotificationReminder` on a booted machine
and hold the host's WAV capture to the shipped master's own samples, decoded on
the host independently of the guest. The lifecycle verticals boot with the
machine's default policy and hold the capture to the startup, login and logout
masters in order, the script stepping on each `MACHINE_CUE_PLAYED` record so no
two overlap. Every other audio vertical plants a machine document turning those
three off, because each holds its capture to one signal.
