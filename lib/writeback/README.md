# tairix-writeback

The write-back core the block-backed filesystem drivers share — ARXFS and
ext4 — so none carries its own copy (`plans/ARXFS-WRITEBACK.md`):

- `DirtySet` — the open transaction's changed blocks keyed by address,
  with a per-operation savepoint so a failed operation undoes alone, and a
  drain in ascending coalesced runs. A block discarded mid-operation moves
  into the savepoint rather than being copied.
- `WritebackBound` — the RAM-derived, pressure-governed, machine-shared
  ceiling on what one volume's transaction may pin, published as a pinned
  ledger.
- `CommitScheduler` — the dirty-age window by device class, reported to the
  host's write-back timer so a quiet volume is still published, and whether
  a deferred transaction will be.
- `RunWindow` — the fallible, zeroised staging buffer one coalesced device
  transfer of at most `RUN_BYTES` moves through, sized at once or grown to
  each run's need.

It performs no I/O; the owning driver does, so every piece is
host-testable.

Stability tier: **experimental**.
