# System notices (`abi-v1`)

A **system notice** is how a process learns that a machine-wide value it
depends on has changed. It is a *state edge*: the subscriber is told only that
the value moved, and then reads the current one. Nothing queues, so nothing
overflows, nothing is dropped, and there is no history to reconcile — which is
exactly right for a value a process must *agree* with rather than witness.

The staged design, including how a topic is added, is
[`plans/NOTICE.md`](https://github.com/tairix/tairix/blob/main/plans/NOTICE.md).

## The three parts

```rust
use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
use tairix_abi::{WaitSetOp, WaitSourceKind};

// 1. Subscribe: an ordinary wait-set member whose `id` is the topic.
tairix_rt::waitset_ctl(
    set,
    WaitSetOp::Add,
    WaitSourceKind::SystemNotice,
    u64::from(NoticeTopic::Desktop.as_u32()),
    MY_TOKEN,
);

// 2. Read, when the park reports MY_TOKEN.
let mut buf = [0u8; NOTICE_PAYLOAD_MAX];
let len = tairix_rt::notice_read(NoticeTopic::Desktop, &mut buf);

// 3. Publish, if this process is the topic's authority.
tairix_rt::notice_publish(&Notice::Desktop(info));
```

No syscall subscribes: `waitset_ctl` already carries the member, so a process
that already parks on a wait-set picks the topic up with one more `Add`.

`notice_read` is unprivileged and never blocks. That matters: the wake lands on
the loop that owes the user a frame, and an IPC round trip there would be the
blocking I/O an interactive surface may not perform.

## Topics

| Topic | Payload | Published by | Read by |
|---|---|---|---|
| `Desktop` | `DesktopInfo` (123 bytes) | the holder of a seat's live display lease | every windowed application |
| `Mounts` | none — the generation *is* the news | the kernel, on every mount-table mutation and every change to the volume behind a mount | the file manager's places rail, PID 1, the device manager, and login's text prompt, which re-checks whether a graphical login can start |
| `MemoryPressure` | the band depth (1 byte) | the kernel, from the pressure gauge | any process holding a reclaimable cache |
| `DisplayLease` | the boot seat's lease word and its holder's login session (`DisplayLease`, 24 bytes) | the kernel, from its seat registry | the display and audio services and the cue authority alone |
| `AudioCapture` | the capture streams moving frames (`u32`, 4 bytes) | the audio service | the session's recording indicator |
| `AudioDevices` | how many times a device or its controls changed (`u64`, 8 bytes) | the audio service | the session, which remembers its user's controls |
| `Network` | how many times an interface's presence, link, addresses or address states changed (`u64`, 8 bytes) | the network stack | the Switchboard, which re-reads the interfaces for the bar's network signal |
| `DisplayOutput` | how many times the display's output arrived, left or ran another mode (`u64`, 8 bytes) | the display service | the desktop session and the login screen, which re-read the output and follow it, and login's text prompt, which gives way to a graphical login once one can start |
| `CueAuthority` | the cue authority's process instance (`ProcId`, 16 bytes) | the cue authority, naming itself | the desktop session, which hands its user's chosen sound files to that instance |

The set is closed and deliberately small. A topic exists only where a *state*
must be agreed; an occurrence a subscriber must witness individually — a
keystroke, a completed transfer — is an IPC message or its own wait source.

## Payload lengths are exact

Every topic carries a payload of one fixed length, `NoticeTopic::payload_len`.
A publish of any other length is refused rather than stored, so a subscriber
never reads a shape the publisher could not have meant. `Notice::encode` /
`Notice::decode` are the one definition both directions use.

`NOTICE_PAYLOAD_MAX` sizes a subscriber's buffer and the kernel's per-topic
retention. It is a containment bound, not a capacity: it is what stops a
topic's payload growing into a channel. It is derived from the closed topic set
as the widest topic's own record — today the desktop's, which carries the
desktop's size and its output's, the scale, the four theme axes, the
double-click interval and the text.

## Authority

Publishing is authorised per topic, and no topic needed a new capability:

- **`Desktop`** admits only the holder of a seat's live display lease — the one
  principal the kernel already attests owns what is on screen, and the same
  fact a `SeatInput` wait-set member and the seat-scoped reserved-endpoint bind
  are gated on. A background session is refused and re-publishes when it
  re-acquires the lease on foreground wake.
- **`AudioCapture`** and **`AudioDevices`** admit only the process bound to
  the reserved `AUDIO_ENDPOINT` — the audio service, which holds every stream
  and every device, so no program can hide its own recording from the
  indicator or fake a change to the devices.
- **`Network`** admits only the process bound to `NETSTACK_ENDPOINT` — the
  network stack, which owns every interface, so no program can fake or hide a
  change to them.
- **`DisplayOutput`** admits only the process bound to `DISPLAY_ENDPOINT` — the
  display service, which owns the output, so no program can fake or hide a
  move of it. It announces at start, resuming one past the count on the board,
  and after every move, before it serves another request.
- **`CueAuthority`** admits only the process bound to `SOUND_CUE_ENDPOINT`, and
  only a payload naming the instance the kernel attests that process is — so
  no program can have a session hand its files elsewhere. The authority
  announces once it has bound, so a new value is also the news that a
  restarted authority holds no session's choices. The kernel's own sentinel
  instance is refused on decode, since no publish can name it.
- **`Mounts`**, **`MemoryPressure`** and **`DisplayLease`** are kernel-owned: a
  userland publish to any of them is refused with `PermissionDenied`.

*Reading* is ungated for every topic but one. `Desktop`, `Mounts`,
`MemoryPressure`, `AudioCapture`, `AudioDevices`, `Network`, `DisplayOutput`
and `CueAuthority` are machine-wide facts no principal owns — whether a sound
device is in use is public on every system, the network and output edges say
only that something moved, never what, and a service's instance is a name to
hand things to, which grants nothing — so gating them would only force
applications to guess at facts the system knows. What the output runs now
stays behind the display service's lease-gated `Query`.
`DisplayLease` is not such a fact: when a console lease is taken and given up
is what the seat inventory reports only under `CAP_SYSINFO_HW`. It is read,
subscribed to and woken for only by the services that follow the seat, the
processes bound to the reserved `DISPLAY_ENDPOINT`, `AUDIO_ENDPOINT` and
`SOUND_CUE_ENDPOINT` — binds only a privileged service can make — and anyone
else is refused `PermissionDenied`. A member whose owner has since given the
rendezvous up reports nothing more.

## Edges and generations

Readiness compares the topic's *generation* with the one the member last
observed; reporting the member ready advances that observation, so the next
wait blocks until the topic moves again. A member added while a topic already
holds an unusual value stays quiet — a subscriber reads the value once at
start-up and is then told only about moves.

Each generation comes from its topic's own source of truth:

- `MemoryPressure`'s generation is the band depth itself, so a band that
  deepens and relaxes again before the waiter runs correctly reports nothing
  to do.
- `Desktop`'s is a counter bumped only when the published record actually
  differs, so re-publishing the current value wakes nobody. A record that moves
  and moves back wakes its subscribers once with nothing changed — one read,
  no repaint — and never misses a real change.
- `Mounts`' is a counter bumped by every mount-table mutation, and by every
  change to what a mount reports though the table did not move: a volume's
  driver registering behind its mounts (an unlocked root), leaving, or
  changing availability. A refused mutation, or an availability set to what it
  already was, changed nothing and bumps nothing.
- `DisplayLease`'s is the lease word itself — four times the boot seat's lease
  generation, plus its phase: held, ended in a handover, or ended back to the
  text console — which only grows, so every acquire, every end, and a console
  switch that ends a handover is one edge. The payload adds the login session
  the holder lies within (`DisplayLease::session`), which changes only with
  the holder. The display service releases a configuration whose lease has
  ended, and lights the display it left dark, without waiting for anyone to
  call it; the audio service holds every stream outside the room the lease
  describes; the cue authority drops the choices of a session that left the
  seat.
- `AudioCapture`'s is a counter bumped only when the published count differs,
  as `Desktop`'s is, and `AudioDevices`' is bumped whenever the audio service's
  change count moves.
- `Network`'s is bumped whenever the stack's change count moves. The stack
  counts a change only when the interface-state records it would answer
  differ from the ones it last published.
- `CueAuthority`'s is bumped when the published instance differs, and an
  instance is never reused, so every start of the authority is one edge.

A counted topic outlives its publisher, and publishing the value in force wakes
nobody, so a restarted service counting from zero could repeat the count its
last instance left and strand every subscriber on that instance's state. Both
publishers take their counts up through `tairix_rt::changenotice::ChangeNotice`,
which resumes one past the count on the board and announces each count once;
the restarted service's first announcement is therefore always an edge.

## The query/edge pairing

An application still *asks* for the desktop before it opens anything (the
window channel's `QueryDesktop`), and converges on the notice thereafter. That
is not two paths for one job: the query answers the value the application needs
before it can size anything, and the notice carries the changes — which is what
keeps a just-spawned application correct without depending on the session
having published before it started. The memory-pressure band has the same
pairing with its System Information query.

## Calling it from C

The generated header carries the topic values, each topic's exact payload
length, the ceiling, and both prototypes:

```c
#define TAIRIX_NOTICE_PAYLOAD_MAX 54u
#define TAIRIX_NOTICE_TOPIC_DESKTOP 0u
#define TAIRIX_NOTICE_PAYLOAD_LEN_DESKTOP 54u
/* ... */

uint64_t tairix_sys_notice_read(uint32_t topic, void *buf, uintptr_t len);
int32_t  tairix_sys_notice_publish(uint32_t topic, void *payload, uintptr_t len);
```

Adding a topic fails the header generator's exhaustive match until the new
topic is named, so the header cannot fall behind the enum it is generated from.
