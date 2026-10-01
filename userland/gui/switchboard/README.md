# tairix-switchboard

The TAIRiX **Switchboard monitor service** (`plans/NEW-TASKBAR.md`
T10–T12): the dedicated, capability-sized process behind the taskbar's
always-right-most Switchboard icon. It samples the live system through the
System Information API, publishes a compact `TraySummary` to the desktop
session over the seat-scoped `SWITCHBOARD_ENDPOINT`
(`lib/abi/src/switchboard_ipc.rs`) which the session binds and the taskbar
renders as the tray signals, and hosts the live overview window that icon
opens.

It is deliberately **not** part of the desktop session's own binary: the
tray overview wants system-wide authority (`CAP_SYSINFO_GLOBAL`,
`CAP_SYSINFO_KERNEL`) that the session's manifest should never have to
carry. The session spawns `switchboard.app` as the logged-in user and reads
its summaries over IPC; the authority lives and dies with this one small
process (`AGENTS.md` §5.2 — capabilities are sized to the holder that
enforces them).

## What it samples

Each cycle gathers one `Sample` (`src/sample.rs`):

- **The process list** — system-wide when `CAP_SYSINFO_GLOBAL` was granted,
  the caller's own processes otherwise. From it: the count of `Stopped`
  processes (the tray's `recovery` signal), the **top task** — the
  process with the highest CPU-time delta since the previous sample, keyed
  on the stable, never-reused `proc_id` so numeric-pid reuse can never
  stitch two lifetimes together — and per process the kernel-attested
  owner uid, mapped bytes, and current scheduling service level. The first
  sample honestly has no top task: there is no interval to measure over.
- **Aggregate CPU time** — the shared `tairix_procinfo::CpuTotals` delta,
  yielding the overall busy fraction in permille.
- **Memory pressure** — the audited `MEMORY_PRESSURE` query (needs
  `CAP_SYSINFO_KERNEL`), on its own slower cadence (below). The published
  level is the honest used-memory fraction,
  `(total - free) * 1000 / total`, and the pressured/normal verdict is the
  kernel's own band (band ≥ 1), whose enter/exit watermarks already carry
  hysteresis.

`derive_summary` (`src/derive.rs`) turns a `Sample` into the wire
`TraySummary`. CPU pressure enters at ≥ 900‰ busy and exits below 800‰ —
the gap is hysteresis so a load hovering at the threshold cannot flap the
tray rail. When both CPU and memory are pressured, the higher level is the
dominant one shown (a tie favours CPU) and the pressure carries the count
of pressured resources. `jobs` is always `0` today: no background-job
registry exists in the OS, and the field stays an honest zero rather than a
fabricated count.

**Honest-data rules.** Every field is a real measurement or an explicit
absence. A denied or failed query degrades exactly the field it backs
(noted once on `stderr`, never spammed per sample); nothing synthesises a
plausible-looking value, and a top-task name that fails wire validation
yields no top task rather than a mangled one.

## The live overview window

The session's `OpenPanel` command shows this crate's own `Switchboard`
screen composition on a requested section, through
`Switchboard::select_section`. `src/view/mod.rs` holds the retained widget
tree, input dispatch, both scroll models and the list geometry every section
shares; one sibling module per section owns that section's view models,
layout, painting and input. `src/panel.rs` owns the window's lifecycle and
`src/model.rs` builds what it shows. The screen is assembled purely from the
shared `lib/controls` controls and paints no chrome of its own — the window
manager decorates the window — and it lives here because it arranges those
controls into one particular window (`plans/NEW-SWITCHBOARD.md` S1). The
window is cut from the icon bar's glass (`WINDOW_GROUND`): its bare ground
lets the blurred desktop through at the bar's weight, and everything on it —
rows, blocks, cards and controls — stays solid. The full design is
`docs/src/desktop/switchboard.md`.

Down the leading edge sits the **navigation rail**: one vertical `Tabs`
strip listing the task list, every resource device under its group heading,
and the recovery list. It is the only route between subjects, so it is never
shed; a rail taller than its column scrolls behind a bar of its own, and
whichever route changes the subject scrolls its entry into view. Beside it
are three sections:

| Section | Source |
|---|---|
| Tasks | the sampled process list, as a sortable table; a secondary press on a row, or Enter on it, opens that task's menu |
| Resources | one pane per resource device the sample names — processor, memory, each storage device, each managed interface, the display path, and the machine's own facts |
| Recovery | stopped processes sampled here, plus the seat report's unresponsive owner ids **joined against those same sampled names** |

Every list scrolls **a pixel at a time**: it is laid out at its natural size
and shown through a viewport, so a row, card or chart scrolled part-way past
is cut by the viewport's edge rather than squeezed, and a wheel detent moves
it the shared wheel step rather than a row. The Resources command rail, which
stays put, lights an Edge Wake while the pane beside it is scrolled away from
its start.

There is at most **one** window. A second `OpenPanel` asks the session to
raise the one already open — naming this service's own pid, since the
session alone owns the window stack — and switches to the requested
section. The window's close control destroys it and the service returns to
headless sampling; **sampling and publishing continue unchanged whether or
not a window is open**. The system is re-sampled strictly on its 2 s
deadline: an input or command wake never re-queries the system.

The panel presents **at most once per wake, and only what the wake's rounds
reported** (`Panel::flush`). Every control the input path reaches reports
the rectangle it repaints into one damage sink the `Panel` owns, which is why
input routes through `Panel::on_pointer`/`on_key`. A composition-wide
transition reports what it re-lays — a scroll its list and its bar, a
subject change the whole client — and a fresh reading reports the
instruments and cells that moved. A change no round could describe (a
resize onto a fresh surface, a desktop appearance or density change, a
session that discarded the retained pixels) calls `Panel::repaint_whole`.

The seat report carries owner **ids only**; the names beside them are the
ones this service attested itself, so display text is never taken from the
wire and an owner this sample never saw contributes no row. A resource that
could not be measured this cycle reads `unknown` with an unmeasured meter,
never a fabricated `0%`.

### Commands, and who may send them

Commands arrive on the per-instance mailbox `command_endpoint_for(<own
pid>)` this service binds: `OpenPanel { section }`, `SeatReport`, `Power {
action }`, `FrameReport` (what the session's last composited frame cost),
`OwnerBundle` (which bundle one window owner was launched from) and
`WatchMachine { watch }` (whether the session's System Monitor screensaver is
up to draw a machine report). The
session's identity is learned from the reply to this instance's first
accepted publish (`decode_publish_reply`), and every command is
authenticated against the **kernel-attested sender of that very message**,
never a claim on the wire. Dropped with a stated reason, before the frame is
even decoded: a command from any other sender, a command arriving before any
session has been attested, and a frame that does not decode.

### The machine report

While the session watches, every sample is also projected into one
`MachineReport` (`src/machine.rs`) and called to `SWITCHBOARD_ENDPOINT` after
the tray summary. The report holds:

- the processors, with every core, and the load;
- committed memory, its band, and the memory composition;
- the task census, with recovery counted by the Recovery section's own
  classifier, and the busiest by processor time;
- the storage devices, least healthy first;
- the interfaces other than loopback, at byte rates.

Each reading is the one the Resources section draws, so the two surfaces
cannot disagree. A name is cut to its wire bound with the shared ellipsis,
never refused.

The session answers `BrokenPipe` once it has no board up, which ends the
watch. Any other refusal ends it too and is stated on `stderr`, while
`WouldBlock` leaves it running. The watch never ends the service.

### Actions

| Control | Effect |
|---|---|
| A Tasks row's menu | `WindowRequest::OpenMenu` for that task; the one `MenuClosed` answering it acts on that task, by identity, as below |
| Task *Switch to* / *Reveal window* | `SwitchboardRequest::ActivateOwner { owner }` to the session |
| Task *Pause* / *Resume* | `signal(pid, Stop)` / `signal(pid, Continue)` — needs `CAP_PROC_CONTROL` |
| Task *Lower priority* | `sched_set_priority(pid, Low)` — needs `CAP_PROC_CONTROL`; spent on a task already at `Low` |
| Task *Force quit* | `signal(pid, Kill)` — needs `CAP_PROC_CONTROL` |
| Resource *Sort tasks by …* | resolved inside the widget: shows the Tasks table ordered by that device's cost |
| Recovery *Restart* | `SwitchboardRequest::RestartOwner { owner }` to the session |
| Recovery *Force* | `signal(pid, Kill)` — needs `CAP_PROC_CONTROL` |
| Window *Close* | destroy the window, return to headless sampling |
| `Power` command | `system_power(action)` — needs `CAP_SYSTEM_POWER` |

A command with no endpoint behind it — *Open logs*, and every resource
command but the sort — is drawn plainly disabled rather than attempted. A task
command its task cannot take is a disabled menu row stating why.

The desktop session holds no power authority of its own: it is the largest,
most exposed process on the seat, so the widest-blast-radius capability in
the system stays out of it and the confirmed choice is relayed here
instead. This service refuses the transition itself when it does not hold
`CAP_SYSTEM_POWER`, before asking the kernel anything, and the kernel checks
the caller again on the far side of the trap. A granted transition never
returns; a refusal names the transition that did not happen on `stderr` and
leaves the machine running. Every tray summary carries a `power_capable`
flag re-read from this service's own effective set at that moment, so the
taskbar renders those rows refused — never optimistically — whenever the
authority is absent, dropped, or not yet published.

Each control's verdict reflects what this service can *genuinely* do: it
reads its own effective capability set through `cap_query` and compares
each row's kernel-attested owner uid with its own (the same rule the
kernel enforces), and the verdict is re-checked at apply time against the
model then held so render and enforcement cannot disagree. A control whose
authority is absent renders with the Authority Mark — a task's menu row
states it as its reason instead — and is never attempted. A sampled task id that does not fit the syscalls' signed width
is refused, never truncated into a different, arbitrary process. A refusal
from the kernel or the session is stated on `stderr`, leaves the model
untouched, and never ends the service — a refused optional action is an
answer, not a fatal error.

## Capability sizing

`AppInfo.toml` requests exactly `CAP_CONSOLE_WRITE`, `CAP_SYSINFO_GLOBAL`,
`CAP_SYSINFO_KERNEL`, `CAP_SYSINFO_HW` (the hardware inventory), `CAP_SHM`
(the zero-copy window frame region the session maps, as for any windowed
app), `CAP_PROC_CONTROL` (signalling a task this service did not spawn),
`CAP_SYSTEM_POWER` (the machine transition the session relays here rather
than performing itself), `CAP_FS_ACCESS` and `CAP_SANDBOX_SPAWN` (reading a
launching bundle's icon and decoding it in a capability-empty worker), and
`CAP_LOG_EMIT` (its own log records). The kernel grants the intersection with
the launching user's ceiling — so an ordinary account's instance simply
publishes that it is not power-capable — and the service probes the optional
sampling scopes **once** at startup (`probe_scopes`) — capability sets are
fixed at spawn, so re-probing per sample could only rediscover the same
answer while spamming the audit log with denied audited queries:

- an **administrator's** Switchboard sees the system-wide process list, the
  memory-pressure gauge, and the hardware inventory;
- an **ordinary user's** Switchboard degrades cleanly to self-scope: its
  own processes, the overall CPU fraction (ungated), no memory signal, and
  no interface or seat inventory.

Either way the service keeps running and publishing what it can honestly
see; a refused scope is an answer, not a fatal error.

## Cadence and keepalive

The run loop is tickless: **one** `waitset_wait` per iteration, parked with
a timeout equal to the time until the next real sample is due
(`src/schedule.rs`). Sampling is strict: a cycle triggered by an input or
command wake before the deadline is a no-op that never re-queries the
system. That single wait covers every source (`src/wait.rs`):
the termination signal, the command mailbox, the machine's memory-pressure
band, and — only while a window is
open — that window's event mailbox, which joins the set when the window
opens and leaves it when the window closes so a closed window's channel is
never left armed. There is no poll loop and no sleep anywhere.

- **Sample period: 2 s** (`SAMPLE_PERIOD_NS`) — frequent enough that the
  tray reads as live, sparse enough that the ungated per-sample queries
  stay a negligible fraction of system load. Deadlines advance anchored to
  the schedule (not to "now"), so the cadence does not drift by the work
  time of each cycle, and an overdue schedule skips the period it missed
  rather than firing a catch-up burst.
- **Memory cadence: every 5th sample** (`MEMORY_SAMPLE_DIVIDER`, i.e. every
  10 s) — the memory-pressure query is audited per call, so its rate is
  bounded independently of the sample period; the reading is carried
  forward between queries.
- **Keepalive: 10 s** (`KEEPALIVE_NS`) — publication is change-only
  against the last *acknowledged* summary, with a keepalive republish so a
  quiet system still proves the service alive. The keepalive doubles as
  orphan detection: an instance whose session died discovers it, at the
  latest, on its next keepalive attempt.

The periodic re-sample is the sanctioned polling fallback: the system-wide
metrics it reads (process CPU times, aggregate totals, the pressure band)
expose no change event to park on, so the service waits the interval on a
one-shot deadline — the CPU sleeps between samples, and there is no tight
re-poll loop anywhere.

## Lifecycle

Spawned by the desktop session after login (never by PID 1). Startup order
in `src/run.rs`: enable signal intake, learn this process's own
kernel-attested identity (`self_origin`), bind the command and window-event
mailboxes under it, build and arm the wait-set (the termination signal is
both the graceful-exit path and a parking source — failure at any of these
is a stated fatal exit), probe the scopes once, then loop sample → derive →
refresh the panel → offer → publish → park.

Exit rules — every abnormal exit states its reason on `stderr` first:

- **Termination signal** → one terse line naming the signal, exit `0`.
- **Publish refused with `NotFound`** (no session bound the endpoint, or it
  exited) or **`PermissionDenied`** (the session refused this instance —
  e.g. an orphan after a session restart) → a stated **clean** exit `0`:
  the service has no purpose without a session to report to.
- **Publish refused with `WouldBlock`** → back-pressure, not a fault, and
  it costs the service nothing. A call endpoint at capacity refuses the
  post outright rather than blocking, so a full queue says only that the
  session has not drained it yet: the summary stays unacknowledged and the
  change gate re-offers it next sample. Counting it towards the give-up
  budget below let a desktop that was merely busy for five sample periods
  kill the monitor watching it — and nothing restarts one, so the tray
  capsule stayed dead until the user pressed it again after the session had
  reaped the corpse.
- **Any other publish failure** → the summary stays unacknowledged and is
  retried next cycle; after 5 consecutive such faults the service exits
  with a stated reason rather than retrying forever.
- **Wait-set failure** → stated exit: continuing without a real park would
  busy-loop.
- **Command mailbox bind refused** → stated exit: a monitor that can never
  be asked to show its overview should say so rather than run on deaf.

## Dependencies and layering

The library is `no_std` (with `alloc`) and consumes only `tairix-abi` (the
wire vocabulary and `Errno`), `tairix-procinfo` (the shared sysinfo client
helpers), `tairix-controls` (the shared controls its own screen composition
is assembled from), and the crates that screen draws through —
`tairix-geometry`, `tairix-theme`, `tairix-raster`, `tairix-input`, and
`tairix-font` — no kernel or driver crate, no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths (`AGENTS.md` §2.9, §17.4).
The `Run` binary additionally links `tairix-rt` (the pure-Rust userland
runtime), the window-channel client, and the font crate's glyph-service
transport, for the bare-metal targets only; on the host it is an inert stub
so workspace-wide builds, clippy, and fmt still cover the file. The screen's
own tests additionally take the controls' shared heavy-contrast theme
fixture through the `test-support` feature, so they assert against the
identical fixture every control's own suite uses rather than a private copy.
Nothing outside `userland/gui/*` depends on this crate (`AGENTS.md` §17.3),
so a headless image omits it cleanly.

Everything with behaviour worth testing is host-tested, with the modules
and their tests side by side under `src/`: the sampler against a scripted
in-memory `Transport` fixture, the screen composition against real window
geometry, theme metrics, and font metrics — so a pixel or a scroll offset a
test observes is the one a user would really have — and the whole run-loop
body plus the window lifecycle against a recording `ServiceHost`
(`src/test_host.rs`) whose wait-set bookkeeping mirrors the production
host's, so the membership assertions are real. The `Run` binary is left
holding only the wiring the host cannot run: syscalls, mailboxes, painting,
and wire-event translation.
