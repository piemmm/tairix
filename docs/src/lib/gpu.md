# `tairix-gpu` — the GPU seam

`lib/gpu` is how TAIRiX reaches a graphics processor for render and compute
work, and what it does when there is none (`plans/GPU.md`). This page covers
the memory and submission model every backend implements and the software
device that implements it on every target. The render and compute vocabulary
arrives with `plans/GPU.md` GP3; reaching a hardware device's driver arrives
with GP11.

## A device and what it is

A `Device` is described by a `DeviceInfo`:

| Part | What it says |
|---|---|
| `heaps` | pools of memory, each with its size and whether it is the device's own |
| `memory_types` | the kinds of memory an allocation is made in, each over one heap, with its `MemoryProperties`: device-local, host-visible, host-coherent, host-cached |
| `queue_families` | sets of queues running one kind of work, `QueueCapabilities::TRANSFER` today, and how many queues of each a context has, at most `MAX_QUEUES_PER_FAMILY` |
| `limits` | the largest allocation, the alignment a buffer's offset takes, and the longest a submission may run |

A description with a memory type over a missing heap, an empty heap, a family
with no queues, too many or no capability, an alignment that is not a power of two, or no
deadline at all is refused (`Error::Malformed`).

`Device::software(total_ram_bytes, clock)` is the software device: one heap of
half the machine's memory, one memory
type that is every kind at once, and one transfer queue running one job at a
time. Its buffers align to 16 bytes, the widest vector its copies move, and its
submissions run for at most a minute.

## Contexts

A `Context` is the unit of isolation. Its allocations, buffers, timelines and
queues are its own:

- **A handle is refused wherever it does not belong.** A buffer, allocation,
  timeline or command buffer of another context, or of another device, is
  refused with `Error::Foreign` where it is named — in a command, a buffer's
  creation, or a submission. A handle carries its device's identity as well as
  its context's, so two devices whose first objects share keys still cannot
  confuse them.
- **A released object is refused** (`Error::Stale`). Objects are filed in
  `tairix-collections`' `SlotMap`, whose generations refuse a key that outlived
  its object however often its slot is reused.
- **Memory is shared only by export.** `Allocation::export` makes a token
  another context of the same device takes in with `Context::import`. The
  memory stays charged to the context that allocated it for as long as anything
  holds it.
- **Dropping the context releases everything it holds.** Its queued work is
  dropped, work already running finishes and is discarded, every thread
  waiting on one of its timelines wakes to `Error::Destroyed`, and its handles
  refuse every request from then on.

## Memory

An allocation is made in one memory type and charged to two budgets: its
context's limit (`ContextDesc::memory_limit`, by default as much as the heaps
hold) and its heap's size. It is refused past the context's limit
(`Error::OverLimit`), then past the heap (`Error::HeapFull`), and only then
attempted, refused if the memory cannot be had (`Error::OutOfMemory`). A
refused attempt takes nothing from either budget. Memory is zeroed when made.

A charge is given back when the memory is finally released, which may be after
its handle: a buffer, an importer, or queued work can still hold it.
`Context::memory_used` reads what the context's allocations take.

The CPU reaches host-visible memory through `Allocation::write` and
`Allocation::read`; memory the CPU cannot reach refuses both
(`Error::NotHostVisible`), as does a range past its end (`Error::OutOfRange`).

A `Buffer` is a span of one allocation, at an offset that is a multiple of the
device's `buffer_alignment`, made for the `BufferUsage` it states:
`TRANSFER_SRC` to be copied from, `TRANSFER_DST` to be copied, filled or
updated into. It holds its memory while it lives.

## Submitting work

A `CommandBuffer` records three commands:

| Command | What it does | Rules |
|---|---|---|
| `copy_buffer` | moves regions of one buffer into another | at most `MAX_COPY_REGIONS` regions; no byte written twice, and within one memory no byte both read and written |
| `fill_buffer` | fills a span with a 32-bit word, little-endian | offset and size whole words |
| `update_buffer` | writes bytes recorded with the command | whole words, at most `MAX_UPDATE_BYTES` |

Recording checks only that what is named belongs to the context. Everything
else is checked when a `Submission` carries the buffer to `Queue::submit`,
**whole and before anything is queued**: every buffer is live and made for the
use, every range is within what it names, no region is empty, every signal
advances its timeline, the deadline is neither zero nor past the device's
longest, and no fixed bound is passed — `MAX_SUBMISSION_BUFFERS` command
buffers, `MAX_SUBMISSION_COMMANDS` commands, `MAX_SUBMISSION_POINTS` waits and
as many signals. A refusal is a `SubmitError` naming the command, counted
across the submission's command buffers, and changes nothing: no byte is
written, no signal is queued, no fence is used. Nothing is clamped into range.

The bounds are security bounds rather than capacities: they cap the work one
request asks of the checker and do not move to admit an oversize submission.

## Order and synchronisation

- **A queue runs its work in the order submitted**, each submission seeing
  everything the ones before it did. A submission waiting for a timeline holds
  back the ones after it on its queue.
- **A `Timeline` only advances.** The CPU sets it with `Timeline::signal`,
  which must pass where it stands and stay short of every signal queued work
  will make; a submission's signal must pass everything queued before it. A
  value is **reached only once every signal up to it has landed**, so work
  completing out of order on two queues never makes a wait see a value whose
  earlier work is still running.
- **A `Fence` is one submission's completion.** `Fence::wait` and
  `Timeline::wait` park on the timeline they wait for and are woken by what
  advances it, the context's loss or its release; nothing spins.
- **A submission may wait for a value no work has yet been asked to signal**:
  it runs once the value lands.

Releasing a timeline while queued work still waits on or signals it lets that
work finish first.

## Deadlines and loss

Every submission carries a deadline, counted from when its work starts. Work
that runs past it **loses its context** (`Loss::Overran`); a lost context
refuses all work (`Error::Lost`), its waiters wake to the loss, and it only
releases what it holds. Every other context's work goes on.

A device engine running several jobs at once is reset when one overruns: the
context of every job past its deadline is lost, as is the context of every
other job that had begun (`Loss::Reset`), since what it had done cannot be
vouched for; the rest go back to wait their turn, ahead of what came after
them. Work the device finds malformed while running it loses its context too
(`Loss::Fault`).

## The software device

The software device runs a job on the thread whose act made it ready — the
submission, the CPU's signal, or the completion before it — or, when its engine
is already running work, on the thread running it, which takes the next job
when it finishes. A job is stopped between two commands once its deadline has
passed; the clock is read for it once a mebibyte has moved since the last
reading, since a reading costs a call into the kernel and a mebibyte's copy a
hundred times that.

Each allocation's bytes have a lock of their own, so the CPU's reads and
writes and the work running over them never touch them at once. A copy between
two memories locks the pair in one order every locker shares. Neither the CPU
nor the work holds the device's lock while it moves bytes.

## Tests

- The model's tests drive engines of any depth through dispatch, completion and
  reset: a reset losing the overrunning and begun contexts and requeueing the
  rest first, out-of-order completions holding a timeline until every earlier
  signal lands, queues served in turn, a queue's order kept with room to spare,
  and descriptions refused.
- The device's tests run the seam on the software device: commands landing
  where asked, handles refused across contexts and devices, export and import,
  refusals that change nothing, ranges refused rather than clamped, overlaps,
  memory limits failing closed after the attempt, freed memory held by queued
  work, order, timelines, deadlines, an overrun losing only its context, and
  waits across threads.
- `tests/fuzz_gpu.rs` (in `cargo xtask fuzz`) records copies, fills and updates
  over buffers sharing allocations, ranges inside, across and past what they
  name, and holds every verdict and every byte to a reference.
- `cargo xtask miri` interprets the crate's tests and the fuzz harness for the
  contention its threads put `lib/rt`'s lock and condition variable to, bar
  the one test that asks the host for an exbibyte, which the interpreter cannot
  represent. The crate forbids `unsafe`; loom has nothing to model, since every
  hand-off is made under a lock.
