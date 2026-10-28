# tairix-gpu

Stability tier: **experimental**

The device-neutral GPU seam (`plans/GPU.md`): the memory and submission model
every backend implements, and the software device that implements it on the CPU
on every target.

## What it provides

- `Device` — a device and its `DeviceInfo`: memory heaps, the memory types over
  them, queue families, and limits. `Device::software` sizes its one heap from
  the machine's discovered memory.
- `Context` — the unit of isolation. A handle of another context or device is
  refused wherever it is named; memory is shared only by an `Export` another
  context imports, and stays charged to the context that allocated it.
- `Allocation` — typed memory, charged to its context's limit and its heap's
  size, refused past either and then refused if the memory cannot be had. The
  CPU reads and writes host-visible memory through it.
- `Buffer` — a span of an allocation made for stated uses.
- `CommandBuffer`, `Queue`, `Submission` — copies, fills and updates, checked
  whole when submitted: a refusal names the command and changes nothing, and no
  range is clamped. A queue runs its work in the order submitted.
- `Timeline`, `Fence` — a value is reached only once every signal up to it has
  landed; a wait parks and is woken by what advances it.
- Deadlines — every submission carries one, and one that overruns loses its own
  context alone.

## Design notes

- The software device runs a job on the thread whose act made it ready: the
  submission, the signal, or the completion before it. Its memory has a lock
  of its own, so the CPU's reads and writes and the work over them never meet,
  and neither holds the device's lock while it copies.
- A refused request makes no change: every check and every allocation a
  submission needs is made before anything is queued.
- No `unsafe`. The crate is enrolled in `cargo xtask miri` for the contention
  its threads put `lib/rt`'s lock and condition variable to, bar the one test
  asking the host for more memory than the interpreter can represent.

## Tests

The model's own tests drive engines of any depth through dispatch, completion
and reset; the device's tests run the seam on the software device, threads
included; `tests/fuzz_gpu.rs` holds the submission path to a byte-array
reference.
