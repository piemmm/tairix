# Desktop sound cues (`cue-v1`)

`lib/abi/src/sound_cue.rs`. A program asks the cue authority for one desktop
sound. A cue is an occurrence rather than a state edge, so it is a request each
recipient witnesses, not a notice: two devices arriving a second apart are two
sounds. The policy around it is [`tairix-soundtheme`](../lib/soundtheme.md).

The rendezvous is `SOUND_CUE_ENDPOINT` (`0x5344_1001`), reserved, so binding it
needs `CAP_IPC_BIND_PRIVILEGED`.

## Requests

Every request opens with the same header:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | magic `CUE1` |
| 4 | 2 | version, `1` |
| 6 | 1 | operation: `1` cue, `2` policy |
| 7 | 1 | reserved, zero |

### Cue

| Offset | Size | Field |
|---|---|---|
| 8 | 1 | `SoundEvent` |
| 9 | 1 | `Answer`: `0` once it starts, `1` once it has played out |
| 10 | 2 | reserved, zero |

The request names only the event and when to be answered. Who may cue it is
decided from the caller's kernel-attested origin, never from anything the
request says (`SoundEvent::owned_by`): a lifecycle event only by the principal
that owns the transition — `Startup` by init (PID 1, which no other process can
ever be), `Shutdown` by a holder of `CAP_SYSTEM_POWER`, `Login` and `Logout` by
the login service's account (`LOGIN_UID`), `DeviceAttached` and
`DeviceDetached` by the device manager's (`DEVMGR_UID`) — and an application
event by a principal whose session holds the room it plays in.

`Answer::PlayedOut` holds the reply until the sound has played out, or until
its drain deadline has passed: the shutdown authority asks so before it powers
the machine off. A cue resolved to silence is answered at once either way.

The cue authority runs as its own service account (`SOUNDD_UID`), which the
audio service admits its tenancies by (`from_cue_authority`); see
[`audio-v1`](./audio.md).

### Policy

The desktop session at the seat hands over its user's sound choices.

| Offset | Size | Field |
|---|---|---|
| 8 | 104 | each event's own file, in `SoundEvent::ALL` order: a read-only `FD_GRANT` handle minted to the authority's instance, or `0` |
| 112 | 2 | document length, at most `POLICY_DOCUMENT_MAX` (2048) |
| 114 | 2 | reserved, zero |
| 116 | length | the settings document, UTF-8 with no control character but `\n` |

The frame is exactly as long as its document says, and `CUE_REQUEST_MAX` is
the longest. The authority admits it only from the anchor of the login session
the seat's display lease names at that moment, and only while it names it; a
document naming a file, or any setting of a sound no user's choices reach, is
refused whole. Every handle is redeemed bound to the caller, even for a refused
push, so none is left pending against the session. Slots may share a handle —
identical delegations are one — and each distinct handle is redeemed once. The
authority's instance is the [`CueAuthority`](./notice.md) notice. The reply is
the status word alone: `SeatNotOwner` from anyone but the seat's anchor,
`OutOfRange` for a document or handle it does not take, or the redemption's own
refusal.

## Reply to a cue

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | status: `0`, or the negative `Errno` |
| 4 | 1 | outcome: `1` played, `2` silent |
| 5 | 1 | `Silence` when silent, else zero |
| 6 | 2 | reserved, zero |

Silence is an answer, not a failure: `Disabled`, `NoAsset`, `Undecodable`,
`Coalesced`, `TooMany` or `NoDevice`. A refusal is `PermissionDenied` for a
lifecycle event from anyone but its owner, `SeatNotOwner` for an application
event from outside the room, and `LimitExceeded` past the caller's cue rate.

Every decode fails closed: a frame of any length but its own, an unknown magic,
version, operation, event, outcome or silence, a dirty reserved byte, or a
silence beside a played cue is refused. A refusal takes the shape of the
operation the frame's header names (`names_policy`): the status word alone for
a `Policy`, a cue reply for anything else.
