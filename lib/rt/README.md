# tairix-rt

The pure-Rust userland runtime: the `_start` entry trampoline, idiomatic
`abi-v1` syscall wrappers, the `entry!` macro, the per-process stack-canary
symbols, and the panic handler that a **first-party TAIRiX program written in
Rust** links. TAIRiX is Rust-only (`AGENTS.md` §1), so its own programs use
this runtime.

## Relationship to the C ABI (`crt0` + `abi-sys`)

`tairix-crt0` and `tairix-abi-sys` are the curated *System runtime / C ABI*
class (`AGENTS.md` §9, §16.4): a libc-equivalent that exists **solely** so a
program **not** written in Rust (C, …) can call `abi-v1`. They are not for
TAIRiX's own code. `tairix-rt` is the Rust counterpart. Both build on the one
shared syscall trap (`tairix-abi-trap`, `AGENTS.md` §2.2), so the trap assembly
is not duplicated, and neither is a privileged path — every capability and
input check happens kernel-side (`AGENTS.md` §5.4).

## Using it

A program is `#![no_std]`, `#![no_main]`, declares its `main`, and hands it to
`entry!`:

```rust
#![no_std]
#![no_main]

use tairix_rt::io::{Stdout, Write};

fn main() -> i32 {
    let _ = Stdout.write_all(b"hello\n");
    0
}

tairix_rt::entry!(main);
```

`_start` validates the kernel-supplied startup vector, installs the
per-process stack canary (`AGENTS.md` §19.2), calls `main`, and routes its
return value through the `exit` syscall.

## Heap (`#[global_allocator]`)

On the three native targets `tairix-rt` registers a `#[global_allocator]`
(`src/heap.rs`) so a first-party Rust program can use `alloc` (`Box`, `Vec`,
`String`, …). It is a free-span allocator over a single contiguous virtual
arena that grows upward, one or more whole pages at a time, by `mem_map`ping
with `MapFlags::FIXED` at the arena's current top; freed regions are tracked as
a coalesced, address-sorted free list held inside the allocator (not as
intrusive links in user memory), so every returned pointer is bounds-checked
before it is handed out (`AGENTS.md` §4). When coalescing frees whole trailing
pages they are returned to the kernel with `mem_unmap` above a **retention**
and in whole **granules** (both below), so the arena shrinks without trading a
syscall pair for every allocation cycle or a syscall per page for a teardown. `realloc` resizes in place wherever it can, avoiding the copy
(`AGENTS.md` §2.16): a shrink always succeeds in place (the surrendered tail
returns to the free list, and top pages above the retention are unmapped if it
reaches the arena top), and a
grow succeeds in place when the bytes immediately after the block are free or
the block abuts the growable arena top. Only when neither holds does it fall
back to allocate-copy-free (copying just the overlapping prefix, leaving the
original block intact if the new allocation fails).
The free-span table is **not** a fixed-size array: it is a capacity that grows
on demand (`AGENTS.md` §24.1 "grow before you fail"). When a workload fragments
the heap past the table's current capacity, the allocator maps one more whole
metadata page (its own `SpanStore` window, distinct from the data arena) and
continues, rather than capping the workload at a hand-picked `const`. Only
genuine resource exhaustion — `mem_map` can no longer supply an arena page *or*
a metadata page — returns a null pointer per the `GlobalAlloc` contract:
deterministic OOM, never a panic (`AGENTS.md` §4 / §2.9). The kernel zeroes
pages on map and on free, so the heap does not re-zero on free; a process
reusing its own freed bytes is not a security boundary (`AGENTS.md` §2.16). The
arena and metadata bases are fixed virtual addresses documented in
`src/heap.rs`.

### Retained top pages

Unmapping every free page at the arena top on each `free` costs a `mem_unmap`
plus the next `mem_map` — two page-table walks, two kernel page-zeroing passes
and a TLB shootdown to every other CPU — for pages the heap is about to want
again. Any allocation high-water that oscillates across a page boundary pays
it, which is every paint loop with a transient buffer: measured at two
syscalls per allocation cycle, and visible in the desktop's frame-budget
reports as hundreds to thousands of syscalls per frame.

The heap therefore keeps free top pages mapped up to a retention, and unmaps
only what exceeds it. The retained pages stay recorded as free, so they are the
next allocation's first fit at the top — which is why they are worth keeping.

The retention is not a constant of the heap's own: it is
`tairix_reclaim::shrink_target` for `ReclaimClass::RuntimeCache` over a
`CacheBudget::from_backing(arena_bytes)`. It therefore scales with the process's
own arena rather than a hand-picked ceiling (`AGENTS.md` §24.1/§24.2) and is
bounded by that budget's documented fraction.

Two things then decide the figure, and they answer different questions. The
budget declares one page as a **working-set floor**, which is what says free top
pages are working set rather than speculation: mild and moderate pressure still
permit them, while severe and critical do not, because the reserved floor stays
zero (`AGENTS.md` §26.3). The result is then **floored at the page granule** —
the unit the syscall is charged at — because the per-band fraction is under a
page for any modest arena, and a sub-page retention is not a smaller cache but
no cache at all.

Conflating those two questions is what made this churn: left to the bare
fraction, moderate pressure retained nothing, so an allocation high-water merely
oscillating across one page boundary paid an unmap and a map every iteration —
two page-table walks, two kernel zeroing passes and a TLB shootdown to every
other CPU, machine-wide, to hand back one page, and worst exactly when the
machine was busiest.

Because `pressure::gauge()` answers `Critical` until told otherwise, a process
that never wires the pressure protocol (below) retains **nothing** and gives
every free page straight back — fail-closed by construction, so opting in is
what earns the retention. `pressure::report` gives the retained pages back on a
band change, which covers a process that has stopped allocating and would
otherwise hold them until its next `free`. The retention is ordinary mapped
anonymous memory the kernel already accounts for, so it needs no cache-report
ledger: it lowers the free-frame count directly, which deepens the band, which
shrinks the retention.

The same figure is the least the arena **grows** by, so a run of small
allocations does not pay a `mem_map` per page either. Mapping beyond what the
caller asked for is speculation about the next allocation, which is exactly what
the band should suppress: the pad is the free top the band already permits the
process to hold, and under real pressure it is zero and the heap maps precisely
the page it was asked for.

### The resize granule

A retention is a **level**, and a level alone does not stop a *teardown*. It
slides down with the free span it bounds: once the free top exceeds it, every
further free of a page finds one more page above the line and hands that single
page back. A descending teardown of a 16 MiB arena cost 3840 `mem_unmap` calls
that way — each a page-table walk, a kernel zeroing pass, the **global**
address-space registry's write lock and a TLB shootdown to every other CPU. One
process's teardown therefore serialised the machine: it was measured in the
desktop's frame-budget reports as 4254 `mem_unmap` calls inside one 275 ms
switchboard frame, alongside a desktop drag frame that spent 992 ms blocked.

So the arena also moves in a granule (`ARENA_RESIZE_BYTES`): a free top is
released only once there is at least that much of it above the retention, and
then all of it goes at once. Sixty-four pages, and deliberately **not** scaled
with the machine, the process or the band, because it is not a capacity — it is
the ratio at which one call's fixed cost stops mattering against the per-page
work that call does anyway. A proportional granule would cost something real: a
sixteenth of a gibibyte arena is 64 MiB of free memory held back from a machine
already asking for it, to save syscalls that had stopped costing anything.

The band has no say in it, because the band decides what may be *kept*, not what
a syscall is worth — and a granule that vanished under pressure would restore
the storm exactly where the machine can least afford it. The price is a residue:
on the allocation path a process can hold up to one granule of free top even at
critical pressure. That residue is what makes `pressure::report`'s trim
**exact** — it ignores the granule and releases everything above the retention
in one call, so a process told the machine is critical still surrenders the lot.

The teardown above now costs 64 unmaps at every band, and its growth 116 maps
where the band permits a pad.

### A refused release is asked once, not once per free

The granule bounds the calls a *successful* teardown makes. It cannot bound a
*refused* one: what the arena wants released is decided by its top and its
retention, neither of which a refusal moves, so the next free finds the same
release still due and asks the identical question again. That is how the same
switchboard frame report survived the granule — around 3800 refused
`mem_unmap`s inside one 250 ms frame, one per `dealloc`, on an interactive
surface. (The refusals were the kernel demanding a release name a whole
`mem_map`, which a shrinking arena never does; `mem_unmap` now releases the
pages a caller holds, `plans/OPEN-DEFECTS.md` D114.)

So a refusal is remembered against the `mapped_end` it happened at, and no
further release is asked until the arena's extent moves — the only thing that
can change the answer. Nothing is lost when one is refused: the pages stay
mapped and recorded as free, so they still serve the next allocation, and the
process simply keeps memory it could not give back.

## Fatal reports

A program that ends abnormally says why, on `stderr` and through the system
log, from fixed stack buffers so neither report allocates. The exit status says
which kind of ending it was:

| Ending | Status | Log event |
| --- | --- | --- |
| A panic | `EXIT_PANIC` (`101`) | `PANIC_REPORTED` (`26000`) |
| An allocation the program could not do without failed | `tairix_abi::OOM_EXIT_STATUS` (`134`) | `OUT_OF_MEMORY_REPORTED` (`26001`) |
| An allocation the program could not do without would pass its own memory bound | `tairix_abi::MEMORY_BOUND_EXIT_STATUS` (`135`) | `OUT_OF_MEMORY_REPORTED` (`26001`) |

A null return from the heap reaches a fallible allocation (`try_reserve`) as an
error. It reaches the allocation-failure handler only for an allocation the
program had no way to refuse, such as a `Vec` growing. The heap keeps why the
failed allocation's pages were refused, so the handler tells the two causes
apart: memory running short, which may pass once memory is freed, and the
program outgrowing its own `address-space-bytes` bound (or pinned budget),
which asking again under that bound cannot cure. Both statuses differ from a
bug's.
The sandbox host does exactly that with a worker's status
(`docs/src/security/sandbox.md`). `lib/rt` owns the `26000..27000` event-id
range.

## I/O abstraction (`io` module)

`tairix_rt::io` is the ergonomic `std::io`-style layer a program programs
against instead of hand-marshalling byte slices: one fd-generic `Read`/`Write`
trait pair (the `read_fill`/`write_drain` transfer loops every other helper —
`read_exact`, `write_all`, `write_fmt`, `File`'s positional helpers, and their
raw-descriptor spellings `fs_read_full`/`fs_write_all` — is built on), the
buffering built on them (`BufReader` with
`read_line`/`read_until`/`lines`, `BufWriter` coalescing small writes), and the
four well-known standard streams (`Stdin`, `Stdout`, `Stderr`, `StdInfo`) plus
a borrowed `Stream` over any descriptor and the owning `File`. It is a pure
layer over the existing `stream_read`/`stream_write` traps — no new syscall,
capability, or `lib/abi` type — so the standard streams, opened files, pipe and
pty ends, and resource references all share one I/O vocabulary (`AGENTS.md`
§2.2).

A kernel refusal is reported as `Error::Os(Errno)` carrying the kernel's own
code, never folded into a zero-length read: `Ok(0)` means end-of-input and
nothing else, so a consumer can never silently truncate what it read because a
capability was revoked or a pipe broke. `Error::as_errno` converts back for an
interface that speaks the kernel's vocabulary. `StdInfo` (fd 3) is the one
deliberate exception — its writes are best-effort and ignorable per `AGENTS.md`
§20.1. Every path is fail-closed, never a panic. See `docs/src/lib/rt-io.md`
and `plans/IO.md`.

## Filesystem (`File`, `Dir`)

`tairix-rt` exposes the userland filesystem surface:
thin `fs_open`/`fs_close`/`fs_read`/`fs_write`/`fs_readdir`/`fs_stat_raw`/
`fs_truncate`/`fs_sync`/`fs_mkdir`/`fs_unlink`/`fs_rename`/`fs_symlink`/
`fs_readlink` wrappers over the `abi-v1` syscalls, the working-directory pair
(`fs_chdir`/`fs_getcwd`; the working directory a relative path resolves from
holds the directory itself, so it follows the directory through a rename),
plus the ergonomic `File` and `Dir` handles a program normally uses. Every
path-only function passes `FD_CWD`, starting a relative path at the working
directory. `realpath` and
`readlink` answer a path's resolution and a link's stored target as owned
text, growing the answer buffer fallibly to the kernel's bound, so no tool
sizes one of its own.
`File` is the one **owning** descriptor handle, whatever its backing (a path, a
resource reference, a pipe end, a pty end — the close trap releases any of
them), and it releases the descriptor on `Drop`, so a handle is never leaked.
It implements `tairix_rt::io::Read`/`Write`, which read and write at the shared
open-file-description cursor: two handles cloned from one description (a spawn
wire, a delegation) walk the file together instead of each restarting it, and a
file is streamed with exactly the same code a program uses on standard input.
`File::read_at` / `write_at` are the **positional** pair — they take an explicit
offset, leave that shared cursor untouched, and split a transfer larger than
`tairix_abi::FS_IO_MAX` across successive syscalls through the same
`read_fill`/`write_drain` loop rather than a second copy of it;
`fs_read_full` / `fs_write_all` are the same pair for a descriptor held as a
number, answering an `Errno`. `File::regular_len` measures a handed-over
document with one `fs_stat` — `None` for anything but a regular file, whose
declared length says nothing about a read. A program names
a descriptor, never a device (`AGENTS.md` §20). Every capability, identity, and
per-inode check stays kernel-side behind the secured VFS (`AGENTS.md` §5.4); a
refusal surfaces as the raw `-errno`. The `open` / `create` / `create_new`
free functions and `Dir::open` are the common-case openers (read-only,
write+create+truncate, write+create refusing an existing file, and
directory-listing respectively).

A `Dir` is also where relative calls start: `open_dir`, `open_file`,
`create_file`, `make_dir`, `mkdir`, `unlink`, `rename`, `link`, `symlink`,
`readlink`, `set_mode_at`, `set_owner_at` and `access` resolve a relative
name from the directory the handle holds, wherever it has moved, and an
absolute one as the free functions do; `chdir` makes it the working
directory. Each runs as the caller. `Dir::create` and `Dir::make_dir` make a
directory and answer it open (`CREATE | DIRECTORY | EXCLUSIVE`), so what is
done to it next reaches the directory made, whatever its name holds by then;
`Dir::unlink` and `Dir::rename` carry an optional `NodeExpect`, refused
`Stale` with nothing changed when the name no longer holds the node it
describes. A descriptor's own node is reached through `EMPTY_PATH`:
`File::set_mode`, `File::set_owner`, `File::access` and `Dir::chdir`.
`access(path, want, flags)` answers whether the caller may read, write or run
a node — its mode, ACL and capability gate under the caller's identity, then
the mount's flags — for a program deciding what to ask or show; the call it
then makes is checked again.

A node is born with the mode its creator names, less the process's file mode
creation mask: `File::create(path, flags, mode)`, `create`, `create_new` and
`fs_mkdir` all take it, `fs_open`'s fifth register carries it, and
`File::open` refuses flags that would create, so no node is created without
one. `umask(mask)` sets the mask (its permission bits; it answers the mask it
replaced) and `umask_get()` reads it, over `fs_umask`; a child inherits it at
spawn.

`replace_file` is the one way a program replaces a whole document, so a crash
or power loss at any point leaves the old contents or the new, never an empty
or torn file: the bytes go to a sibling temporary named for that replace alone,
made owner-only and held open, which takes through its descriptor the
original's extended attributes (through the copy engine, every one or the
replace is refused), owner, group and mode, is synced, and is renamed over the
original only while its name still holds it; a second sync puts the rename on
the medium.
`staging_name` draws that sibling's name — `<name>.<16 hex digits>.new`, or
`.<16 hex digits>.new` in the same directory when the name would outgrow a
component — for any program that builds a replacement itself, as `mv` builds
a cross-volume move, so every staged replacement is spelled alike.
`ParentDir::CreatedIfMissing` creates the document's directory only when the
replace finds it missing. A document that did not exist is created with the
mode the caller names; the temporary replacing one that did is its writer's
alone until it is whole, so the bytes and attributes it takes first are never
shown to anyone, and a link swapped in at its name is never written through
nor put in place. A file's ACL and capability gate are not visible to
a program, so neither survives a replace: it is not for a file carrying
either. `replace_with_link` is the same rule for a name a link replaces —
`ln -f`, `cp -lf`/`-sf`, `fstree` copying a link onto a file the user agreed
to replace: the `NewLink` (symbolic, or a second name for a node) is made
under such a sibling and renamed over the name, so the name is never missing
and nothing it held is removed unless the link was made; a hard link's
missing source is refused before the name is touched.

The `copy` module is the shared tree seam and the copy engine's data seams
(`lib/fscopy`) over this runtime: a `File` is a held node, read and written
with its holes through its descriptor; a `Dir` is a held directory whose
children are reached by name from it, never through a link
(`NO_FOLLOW`); and `WorkingDirectory` opens a tool's operands from the
working directory. `Dir::open_dir` opens a child directory the same way, so a
link where a directory was expected answers `NotADirectory`.

`File::open` is the one **open-by-name** path, and it applies the shared
`lib/resref` spelling rule (`names_resource_reference`) before any lookup: a
filesystem path goes to `fs_open`, while a resource reference (`sys:random`,
`sys:null`, …) goes to `resource_open`, the kernel's capability-checked
resource resolver. Because the routing lives here, **every** first-party
program accepts a resource reference wherever it accepts a file name —
`cat sys:random`, `tee sys:null` — with no tool-side code (`AGENTS.md` §2.2).
A spelling that names a reference is never retried as a filesystem lookup
(the kernel resolver's refusal stands, fail closed), an on-disk name
containing `:` stays reachable as `./name`, and `File::open_resource` remains
the explicit constructor for a caller that has already classified its target
(the shell's parsed redirection targets).

Extended attributes are reached through a descriptor only: `File::attr_keys`,
`attr_value`, `attr_set` and `attr_remove` act on the node the handle holds,
a link's own when it was opened `NO_FOLLOW`. `attr_keys` and `attr_value`
are the shared drains of the reads: the first walks `fs_attr_list`'s index
iteration to its end-of-list answer (refusing a key the backing spelled
malformed rather than showing it lossily), the second reads one value into a
buffer the fixed `FS_ATTR_VALUE_MAX` bound guarantees is large enough. Every
consumer — the file manager's Properties window, `fstree`'s attribute editor
and the copy engine's seam — reads through them, so no tool carries its own
copy of the iteration rule.

## Raw syscall results (`Errno::from_syscall`)

The low-level wrappers hand back the kernel's raw signed register, so a
refusal arrives as a negated `Errno` discriminant. `Errno::from_syscall` in
`lib/abi` is the **one** place that becomes a typed error — this runtime's own
wrappers, the driver programs that issue syscalls directly, and every
application all recover their `Errno` through it, so a refusal cannot be read
one way in one program and another way in the next. It lives beside `Errno`
rather than here because a raw result reaches consumers that have no reason to
link a runtime.

Anything unrecognisable — a code this build has no variant for, a magnitude
too large for an `i32`, `i64::MIN`, or a success value handed in by mistake —
fails closed as `Errno::NotImplemented`. It deliberately never becomes
`Errno::NotFound`, which asserts a named object is absent and which callers act
on; an unreadable result must not be able to masquerade as that answer.
`Errno::try_from_syscall` is the fallible form, for the caller that must tell
an unreadable register from a real refusal instead of folding the two.

## Memory pressure (`pressure` module)

`tairix_rt::pressure` holds the process's single
[`ReportedPressure`](../reclaim/README.md) gauge (`plans/SMARTRAM.md`
SMART5): the process-wide answer to "how tight is memory" that every
`tairix_reclaim::ReclaimCache` in the program consults, so they all shrink
on the same band at the same moment. A userland process cannot measure free
frames, watermarks, or the reserve floor itself, so `pressure::gauge()`
answers `Critical` until `pressure::report(band)` tells it otherwise —
admitting nothing rather than assuming the machine is comfortable.

The runtime deliberately does not fetch the band itself: reading it needs a
System Information endpoint and a transport the runtime has no business
choosing for a program. The owning program parks on the ungated
`MemoryPressure` system-notice wait-set member, reads the band with the
ungated `SysinfoQueryId::MEMORY_PRESSURE_BAND` query on wake, and calls
`pressure::report` — event-driven throughout, never polled (`AGENTS.md`
§2.23).

## Reporting the process's caches (`cachereport` module)

The band comes *in* through `pressure`; the figures go *out* through
`tairix_rt::cachereport`. A process's heap is its own, so nothing outside
it can measure what its glyph atlas or its decoded icon artwork is
holding — left unreported, the `disposable-ui` reclaim class would read
zero on a monitor while a desktop held megabytes of exactly that. Each
cache hands the runtime a `tairix_reclaim::CacheLedger` with
`cachereport::register`, and the runtime submits the set through the
ungated, self-scoped `SysinfoQueryId::CACHE_REPORT` operation. Being
ungated costs nothing: the process describes only itself, grants nothing,
and reads nothing, and the service stamps the kernel-attested identity
rather than believing the caller's.

The owning program drives it, for the same reason it drives `pressure`.
Once per turn of its event loop it calls `cachereport::publish_if_due()`,
which samples the registered caches and sends only when the sample
*differs* from the last one sent and the minimum interval has elapsed —
the comparison is the change detection, so there is no dirty flag to
forget. When the interval suppresses a change,
`cachereport::wait_deadline_ns()` returns the nanoseconds remaining and
the program folds that into its wait timeout, so exactly one bounded wait
is armed and only while something is genuinely pending. An idle process
arms nothing and sends nothing: its last report is still true, because a
process that is not running is not changing what it holds. There is no
timer and no poll loop on the path. `cachereport::ReportGuard`, held for the
scope in which the caches are registered, removes the process's rows on every
way out — a clean return, a fail-loud exit, a panic unwind — so a monitor never
shows memory nobody holds. Every publisher holds that one guard rather than its
own copy of the same three lines.

The report is **handed over, never awaited** (`submit` module, below). Its
publishers are a desktop compositor, a file manager and the font service, each
owing somebody a frame or an answer, so a blocking `ipc_call` here parked the
caller off the run queue for a full cross-process round trip — twice a second
per publisher — which the desktop showed as a stutter through every gesture.
The withdrawal is the one report that waits, because the kernel drops a posted
request whose poster has exited and a withdrawal that did not land would leave
a monitor showing memory nobody holds.

## Submitting a figure without waiting (`submit` module)

`tairix_rt::submit::Submission` is the one shape for a statement a process
makes about itself: `call_post` hands the request over and returns, the caller
carries on, and `call_reap` collects the verdict without blocking on a later
pass. One submission is outstanding at a time — a restatement of a figure has
nothing to say until the last one has landed — and each carries a deadline, so
a wedged or absent service costs one abandoned ticket rather than a blocked
loop. `cachereport` above and the desktop session's frame accounting both go
out over it, so neither hand-rolls the ticket bookkeeping.

## Announcing a change count (`changenotice` module)

`ChangeNotice` is a counted notice topic's publishing side — `AudioDevices` for
the audio service, `Network` for the stack. The kernel keeps a topic's value
past its publisher and wakes nobody for a re-statement of it, so
`ChangeNotice::resume` answers the count to go on from: one past the value on
the board. A restarted service therefore never announces its last instance's
count, and its first announcement always reaches the subscribers.
`ChangeNotice::announce` publishes each count once.

## Targets

The `_start` trampoline, stack-canary symbols, and panic handler are compiled
in only for the three native Tier-1 targets (`x86_64-tairix-none`,
`aarch64-unknown-none`, `riscv64gc-unknown-none-elf`), selected by a
build-script-emitted cfg (`rt_native_<arch>`) rather than `cfg(target_arch)`
so the instruction-set choice stays out of the source the §17.2 `cfg-check`
guards. `wasm32` has no trap instruction and is out of scope
(`plans/CCOMPAT.md` §1). On the host only the syscall-wrapper marshalling is
compiled, unit-tested through the trap crate's injectable seam.

## Stability tier

`experimental` — `abi-v1` is **not** frozen yet (`plans/CCOMPAT.md` §0). The
exposed syscall-wrapper surface grows as TAIRiX programs need it: the
standard-stream wrappers (`stdout`, `stderr`, `stdinfo`, `stdin`, `AGENTS.md`
§20), `spawn` / `spawn_in` / `spawn_attached` / `spawn_attached_in` (the child
started in a directory resolved as the child) / `console_count` / `wait` /
`yield_now` / `exit`,
the anonymous-memory pair (`mem_map`, `mem_unmap`) and the `mem_map`-backed
`#[global_allocator]` they power, the resource-limit pair (`rlimit_get`,
`rlimit_set`), the session wrappers (`set_input_mode`, `users_db_read`,
`key_inject`, `keyboard_read`, `display_acquire` / `display_release`,
`ipc_send`), the user-space-driver wrappers (`mmio_map`, `dma_alloc`,
`resource_grants`), and the monotonic clock (`clock_get`) plus the timed park
built on it. That park is one definition with two entry points (`AGENTS.md`
§2.2): `park_ns` for anything that already holds a nanosecond span — an
animation's next frame, a timed retry — and the `ClockDelay` `Delay` facility
for a driver's microsecond settle window. It genuinely sleeps, in a futex
wait on a word nothing wakes, so the kernel's one-shot timer wakes it and no
kernel object a limit could refuse is needed. `park_forever` is its unbounded
counterpart.

`PrivatePort` and `WaitSet` own a process-private message port and a wait-set,
releasing them (`port_unbind`, `waitset_close`) when dropped, so a
per-operation transport returns what it was charged; `WaitSet::wait_until`
parks until a member is ready or an absolute deadline passes.

`latency_watch` is the one diagnostic wrapper: an interactive surface calls
it once with `tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS` before its event
loop, and the kernel then reports any span that overruns, naming the syscall
that spent the budget and the user stack that led there
(`docs/src/architecture/latency-diagnostics.md`). It returns the budget
actually armed, which is `0` on an image that compiles the diagnostics out —
an answer rather than a failure, so a surface neither branches on the image
it runs in nor reports anything to the user.
