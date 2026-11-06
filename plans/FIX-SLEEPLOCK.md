# FIX-SLEEPLOCK — A wake decision applies to the park it was taken about

Status: **done** (S1–S7 landed; gate status in the change that lands them).

Binding under `AGENTS.md`. Two defects, one root cause: a wait-queue row is
identified by task id alone, so a decision taken about one park is applied to
a later one — and a row whose task can never run again is still counted as a
waiter. The first strands a live task on a free lock; the second eats a
counted wake.

Read first: `kernel/core/src/sleeplock.rs`, `kernel/core/src/waitq.rs`,
`kernel/core/src/futex.rs`, `kernel/core/src/threads.rs` (`retire`),
`kernel/sched/api/src/policy.rs`, the three `kernel/sched/*/src/scheduler.rs`
`unpark` / `wake_from_parked` pairs, and `plans/OPEN-DEFECTS.md` D112 — whose
fix introduced the delete this plan makes precise.

## S1 — the strand (`OPEN-DEFECTS` D129)

SMP-only. At `-smp 4` the boot reaches the framebuffer `ARXFS passphrase:`
prompt and stops: no `/Drivers/input/*` bundle loads, so no keyboard works,
and all four cores end in `wait_for_interrupt` with the disk idle. ~50–60% of
runs at `-smp 4`, never at `-smp 1`. Two shapes: the driver-store scan kthread
strands (no store scan, service bundles still load), or an `fs_*` bundle loader
strands *holding* the per-mount `SleepLock` and the rest queue behind it (all
19 scan candidates logged, zero `application bundle loaded`).

`SleepLock::hand_off_oldest` reads, wakes, retracts and deletes across four
separate acquisitions of the wait-queue lock, naming its subject by task id:

```rust
while let Some(task) = self.waiters.oldest_task() {   // lock dropped here
    self.handoff.store(task, Ordering::Release);
    if self.waiters.wake_task(hook, task) { return true; }
    if self.retract_handoff(task) { return true; }
    self.waiters.deregister(task);                    // whatever row is there now
}
```

The reachable interleaving, with contender `T`:

1. Releaser reads `oldest_task()` → `T`, holding registration *A*.
2. Releaser publishes `handoff = T`.
3. `T`, resumed by an unrelated wake, deregisters *A*.
4. `wake_task(T)` finds no row and reports `false`. `unpark` is never called.
5. `retract_handoff(T)` succeeds — `T` has not reached its claim.
6. `T` fails its claim, fails `try_lock` (the handoff left `LOCKED` set),
   registers *B*, and parks.
7. `deregister(T)` deletes ***B***. The queue is now empty, so
   `release_and_recheck` clears `LOCKED` **and** `CONTENDED`.

`CONTENDED` clear means every later release takes the one-compare-exchange
fast path and never consults the queue again. `T` is parked for ever on a free,
uncontended lock. Whichever disk consumer was designated at that moment
strands, and anything queued behind a mount lock it held strands with it.

A second, independent hole makes the same delete unsound even when the row
identity is right: `Scheduler::unpark` returns `Err` when
`wake_from_parked`'s `Parked → Ready` compare-exchange loses to a concurrent
waker — a **live, runnable** task. `SchedWaitQueueArch::unpark`
(`kernel/core/src/init.rs`) documents the opposite ("an error means the task
can never run again"), and `hand_off_oldest` acts on that reading.

## S2 — a retired task's rows (`OPEN-DEFECTS` D130)

Nothing deregisters a task from a wait queue on its behalf: every waiter
removes its own row on its own way out. A task killed while parked therefore
leaves a row in every queue it was registered on, and scheduler ids are drawn
at random and never reused, so the rows accumulate for the life of the boot.

That is not only a leak. `wake_in_arrival_order` (`wake_one` / `wake_n`) and
`wake_key` count the unparks they *issue* and discard each result:

```rust
woken += batch.len();
for &id in &batch { arch.unpark(id); }
```

So `wake_n(arch, 1)` over a queue whose FIFO head is a retired task's row
reports `1` and wakes **nobody**. On the futex path — where `FUTEX_WAKE(n)`
releases a caller-chosen count — that is a lost wake-up: a live waiter stays
parked because a corpse spent its wake. `futex::deregister` drops a queue once
it is empty, so the corpse also pins its bucket entry alive.

## What the fix guarantees

1. **A decision about a registration applies to that registration.** A row is
   named by `(key, task, seq)`, so a waiter that resumed, deregistered and
   parked again is a *different* registration and inherits nothing.
2. **A row is removed only by its own waiter or after the scheduler says its
   task can never run.** Nothing else deletes another task's row.
3. **`unpark` reports `Err` only for a task that can never run again** — it is
   terminal, or the id names none. A wake that another waker's wake already
   satisfied is success.
4. **A counted wake counts landed wakes.** `wake_n(arch, n)` releases `n` live
   waiters or exhausts the queue; it never spends a slot on a row it could not
   wake.
5. **A retired thread leaves no rows anywhere.** Retirement drops them, and a
   queue the kernel cannot name self-cleans on its next wake.
6. **D112 stays closed.** A genuinely retired row is still reaped rather than
   left at the head of the queue, and a handoff is never published to a task
   that cannot claim it.
7. **Waiting for the lock never needs the heap.** A contender stands in line
   through the row its thread was made with, so an acquire on an exhausted
   heap still parks and is handed the lock (S7).

## S3 — registration identity in `WaitQueue`

`WaitSet` already mints exactly the generation this needs and does not expose
it: `next_seq` is monotonic and never reused, a re-`register` of a *present*
row keeps its `seq` (so a re-arming handler keeps its FIFO place), and a
`deregister` followed by a `register` mints a fresh one. So `seq` *is* the
registration identity; only the API is missing.

- `Registration { key, task, seq }` — private to the queue. The sleeping
  lock's line names a stand the same way, by its row and the row's join count
  (S7).
- `oldest_registration() -> Option<Registration>` reads the line's head
  without removing it.
- `wake_registration(arch, &reg) -> bool`: require the present stand or row
  to be the one `reg` names — a mismatch or an absent one returns `false`
  having touched nothing, because the designated waiter has resumed and is
  either about to claim or about to stand again. Otherwise `unpark` with no
  lock held, and on a refusal re-take the lock, re-check the identity, and
  reap.
- One removal, `WaitSet::remove(id) -> Option<Waiter>`, serves
  `deregister_keyed` and the reap.

`hand_off_oldest` becomes a scan over rounds, and one round — `offer_to` —
publishes ownership, wakes, and withdraws on a miss. It removes nothing:

```rust
while let Some(reg) = self.waiters.oldest_registration() {
    if self.offer_to(hook, &reg) { return true; }
}
```

The round is its own function because it is also the only seam a test can
drive with a *stale* designation: nothing else can place a re-park between
reading the queue and acting on what was read. `retract_handoff` now reports
whether it withdrew (it used to return `true` for "did not withdraw", which is
how a predicate named for its action came to mean the opposite).

**It terminates.** Every round either returns or changes the head: the row is
reaped, or its waiter resumed, or a newer registration stands in its place.
No row can appear below the current minimum `seq` — a fresh `register` takes
`next_seq`, which only rises — so the head's `seq` strictly increases across
rounds and the scan cannot revisit one.

## S4 — one park/unpark handshake, with an honest error contract

The park state machine is lifecycle, not scheduling policy, so it is one
definition rather than three. Only the *placement* differs, and placement is
policy.

- `kernel/sched/api::park` holds the handshake — `unpark_task` and the
  dispatch's `settle` — parameterised on the policy's own "admit this woken
  task" step, over the trait each per-policy `TaskInner` implements
  (`load_lifecycle` / `cas_lifecycle` / `swap_lifecycle`). The wake token is a
  bit of the lifecycle word, so the settle that would park consumes it in the
  same exchange; a token kept in a word of its own could be taken by a CPU
  still finishing an earlier park of the task (`OPEN-DEFECTS` D994).
- Each policy keeps `placement_for` / `admit_fresh_on` (cfq, eevdf) and
  `preferred_home` / `push_class` (mlfq), and nothing else of the handshake.
- The compare-exchange-loss arm re-reads the state: `Exited` is
  `Err(InvalidState)`; anything else is `Ok(())`, because the task is runnable,
  which is what the wake asked for. Bounded — no retry loop.
- **Delete the `cpu_state(target)?` that sits after the state commit** in all
  three. It is the only other way `unpark` can report `Err` for a live task,
  and in mlfq it additionally *loses* the task: `Ready`, unqueued, and an error
  returned. `admit_fresh_on` already routes an out-of-range CPU to the overflow
  list, which is the fail-safe that check was reaching for.
- `SchedulerPolicy::unpark`'s doc states the contract the callers rely on: a
  wake of an already-runnable task is `Ok`.

## S5 — no rows outlive their thread

`threads::retire` is documented as the per-thread half of **every** death, and
already clears the signal intake and the kill gate there. Dropping the thread's
wait-queue registrations belongs beside them, before the scheduler releases the
id.

- `WaitSet` gains `by_task: OrderedSet<(TaskId, WakeKey)>`, so one task's rows
  are a contiguous range and `WaitQueue::deregister_task(task)` is
  O(log n + rows) rather than a scan of `by_waiter`. It is the fourth
  cross-index, maintained with the other three and for the same reason.
- `futex::deregister_task(thread)` walks the fixed bucket table, calls
  `deregister_task` on each live key's queue, and drops the ones it empties.
  A bounded walk over a fixed table, paid once per thread death.
- **One list of queues, not three.** There is today no list naming every
  global queue: `TIMED_QUEUES` names nine and `DEFERRED_WAKE_QUEUES` five,
  and a third hand-maintained list would be a third thing to forget a queue
  in. Replace both with one `ALL_QUEUES` of `{ queue, timed, deferred }`
  entries; `nearest_timed_deadline` and `drain_pending_wakes` filter it on
  their own flag, and retirement walks all of it. `WaitQueue::new` stays
  `const`, so the statics are untouched.
- Every wake path reaps a row whose `unpark` reports the task can never run —
  `wake_in_arrival_order`, `wake_key`, `wake_waiter`, `sweep`, and
  `wake_registration`. This is what covers a queue retirement cannot name: a
  `SleepLock`'s line is embedded in the mount or device that owns it,
  reachable from no registry, and is dropped with its owner.
- `wake_in_arrival_order` and `wake_key` count a waiter only once its `unpark`
  has landed, and continue past one that did not, so a counted wake cannot be
  spent on a corpse (guarantee 4).

## S6 — the SMP coverage the gate does not have

`autoload-input-qemu-aarch64` is enrolled with `cpus: 1`
(`tools/xtask/src/commands/qemu_tests.rs`), so the whole
unlock → driver-store scan → autoload chain is only ever proven single-CPU —
which is why nothing in the gate has ever seen this. That vertical is *not*
the place to fix it: its 300 s TCG budget already covers boot, a bounded
PBKDF2, autoload, typed login, two app spawns and a pty round trip, and it
carries open defect D15 (an intermittent single-CPU freeze at its Ctrl-C
stage), so an SMP failure there would be ambiguous and a 4-vCPU budget miss
would be the load-dependent timeout §7 forbids.

The dedicated `cpus: 4` row boots the **same vertical** as
`tairix-test-netstack-autoload-qemu-aarch64` — unlock → store scan → autoload
a signed driver into its own user process → `devmgr` binds it to `netstack` —
as its second binary, `tairix-test-netstack-autoload-smp-qemu-aarch64`, whose
tree describes four CPUs: the aarch64 boot starts only the cores its tree
names, so the one-CPU tree under `-smp 4` would run on one. `sidecar_path`
disambiguates each enrolment's planted image and serial log by its `TESTS`
index (`sidecar_paths_never_collide_across_enrolments_or_replicas` pins it). That
chain rather than the graphical one because it reaches the same store scan and
user-space driver spawn without the desktop and pty stages, so a failure there
cannot be confused with D15.

The budget needs no headroom for the extra vCPUs: `timeout` is the
inactivity window, not a runtime deadline, and a strand shows up as silence.

## S7 — a contender waits without the heap (`OPEN-DEFECTS` D1682)

`SleepLock::lock` cannot fail, and a lock is contended where memory is short
— a filesystem writing back to make room — so filing a contender in a
`WaitQueue`'s indices, which allocate, aborted the kernel on an exhausted
heap. The lock keeps a line of its own instead:

- **`LineRow`**, one per thread: allocated with its `ThreadControl` by
  `kthread::spawn_control`, which already refuses `OutOfMemory`, and dropped
  with it. A thread waits on one lock at a time, so one row serves every lock.
  The running task's row is reached through its published resume handle
  (`ResumeHandle::line_row`, behind `WaitQueueArch::line_row`).
- **The line**: an intrusive FIFO threaded through the rows, `Arc` forward and
  `Weak` back, so joining and leaving allocate nothing. The line holds a share
  of each row in it: a thread that dies waiting leaves its row until a release
  reaps it, and a dropped lock splices its rows out one at a time.
- **Identity**: a stand is the row and its join count, which every join
  raises, so S3's guarantee holds for the line — a decision about one stand is
  never applied to a later one, and the reap removes a row only under that
  identity after `unpark` refuses. The scan terminates: while it holds the
  lock no waiter can own it and leave, so the head changes only by the scan's
  own reaps or by a waiter claiming the handoff, which ends the scan.
- **SMP**: every link changes under the line's spin lock; a row's own lock is
  taken only inside it, one row at a time, by the one line the row stands in,
  so no lock order inverts and no two lines touch one row. The handoff word and
  `CONTENDED` are unchanged, and the uncontended acquire and release are one
  compare-exchange each.

A task with no row has no resume handle published either, so it cannot park:
it takes the unscheduled spin, as a caller before the boot installs a hook
does. A waiter parks through `waitq::park_calling_task`, the one park every
kernel wait takes, so a host test's boot blocks it as it blocks the rest.

## Tests

- **`sleeplock.rs`** — `a_designation_the_waiter_outran_deletes_nothing`
  drives `offer_to` with a designation its waiter has already left and
  re-parked behind, and asserts the live row survives and is handed off on the
  next round; `a_release_never_clears_the_word_over_a_live_waiter` asserts the
  invariant over a whole release; `a_reap_only_removes_the_registration_it_was_taken_about`
  covers the reap's identity check against a wake that reports "did not land"
  for a live task. The D112 tests still pass — a genuinely unwakeable row is
  still reaped and a live successor still reached.
- **S7** — `a_contended_acquire_on_an_exhausted_heap_parks_and_is_handed_the_lock`
  refuses the heap for a whole contended acquire, which parks and is handed
  the lock; `a_line_stays_whole_under_a_reap_racing_a_rejoin` races joins and
  leaves against a reaping release on two threads; `kthread`'s
  `a_published_kthread_answers_the_row_it_was_made_with` pins the row's reach.
- **`waitq.rs`** — `a_counted_wake_never_spends_itself_on_a_row_it_could_not_wake`
  (the S2 lost wake-up) and `a_retiring_task_leaves_no_row_on_any_key`.
  `an_addressed_wake_reports_the_landing_not_the_registration` additionally
  asserts the reap.
- **`kernel/sched/api`** — `park.rs`'s own model tests pin the handshake,
  including the lost-claim arm that is only reachable concurrently
  (`a_wake_whose_claim_lost_to_another_waker_still_reports_success` is the S4
  regression test); the `unpark_errs_only_for_a_task_that_can_never_run`
  conformance vertical pins the contract for all three policies through
  `run_all`.
- Each of the four defect tests fails against the pre-fix behaviour and passes
  after; verified by restoring that behaviour and re-running.

The test the plan expected in `futex.rs` is not written: the futex half is the
same `WaitQueue` code the `waitq.rs` pair already covers, and a third copy of
it addressed through `futex::deregister_task` would assert the bucket
bookkeeping, not the defect. The bucket half is covered by that function's own
`retain` dropping an emptied key.

## The oracles

- **miri.** The sleeplock's `unsafe` is its `Send`/`Sync` impls and the
  guard's `Deref`; its line is safe code over `Arc`, `Weak` and spin locks.
  The one `unsafe` S7 adds is the resume handle's row thunk
  (`kthread::line_row_thunk`, `ResumeHandle::line_row`), under the handle's
  raw-pointer protocol. `kernel/core` is enrolled (`OPEN-DEFECTS` D123), so the
  stage interprets the sleeplock's tests and the thunk's.
- **loom cannot reach this crate yet.** `--cfg loom` does not compile
  `kernel/core`: loom's atomics and cell have no `const` constructor, and
  `WaitQueue::new` / `SleepLock::new` are `const fn`, so the twenty-three global
  wait queues are plain `static`s. Nor would building it be enough: the sleeping
  lock's `state` and `handoff` are `core` atomics behind no loom shim. Enrolling
  the crate means routing those words through one and declaring the statics
  through `tairix_sync::global!` and `const_fn!`, its own piece of work. The
  interleavings this plan turns on are therefore driven deterministically by the
  tests above, at the exact windows, rather than searched — and that gap is
  recorded as D1643.

## Decisions, and what is deliberately not taken

- **The corpse reap stays.** D112's wedge was a handoff published to a task
  that could never claim it, and the reap is what keeps a dead row off the head
  of the queue. The fix is to make it precise, not to remove it.
- **A stale designation is retried, not repurposed.** Waking a task whose
  designated registration is gone would often work — it is still a waiter on
  this lock — but it conflates two parks again. The rescan designates its new
  registration on the next round and terminates (S3).
- **No per-task list of queues.** A back-pointer from a task to a
  `SleepLock`'s embedded queue would have to outlive a mount it cannot; the
  `by_task` index plus the one `ALL_QUEUES` walk covers every queue the kernel
  can name, and the wake-path reap covers the rest.
- **A stand is private to the line.** It is the sleeplock's ownership-transfer
  contract with its own line, not ABI, and nothing outside the module
  designates a waiter.
- **A row per thread, not a pool.** A pool the locks shared would need
  retirement to know whether a dying thread's row still stands in a line; a
  row its thread owns and the line shares needs no such account.
