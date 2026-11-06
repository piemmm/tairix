# `tairix-writeback`

The write-back core shared by the filesystem drivers that batch their
commits, [ARXFS](../filesystem/arxfs.md) and [ext4](../filesystem/ext4.md).
A driver holds the open transaction's changed blocks in RAM and publishes
them behind its own barriers; this crate holds the pieces of that which
are the same for every format.

- **`DirtySet`** keys staged blocks by address in two phases (before and
  after a barrier). `begin_operation` opens a savepoint; the first change
  an operation makes to a block saves what stood there, so
  `undo_operation` restores the transaction exactly as it was. A staged
  block is filled from the device the first time an in-place edit touches
  it (`stage_with`), one staged for the other phase moves across whole unless
  the savepoint keeps it as it was, and `drain` writes a phase out in
  ascending runs of at
  most `RUN_BYTES`. Every buffer is wiped as it is released. The set lives
  in `tairix_collections::OrderedMap`, so a change the heap refuses answers
  `OutOfMemory` and leaves the set as it was; each change first makes room
  for what undoing it would put back, so the undo asks the heap for
  nothing, and a set drained empty gives its room back.
- **`WritebackBound`** derives a volume's ceiling from the RAM the boot
  path discovered, folded through the system pressure gauge and capped by
  the share every mounted volume draws on, never below one coalesced
  transfer; a machine that cannot spare that much refuses the mount.
- **`CommitScheduler`** keeps the transaction's age against the window
  `writeback_window_ns` gives what one command to its device costs
  (`BlkCommandCost`: 30 s bridged, 15 s seeking, 5 s cheap; halved per
  pressure band) and reports the deadline to
  the host, whose timer flushes a volume that falls quiet. `defers` says
  whether a transaction left open will be published that way; without a
  host or a clock, a driver publishes every operation. `postponed` dates a
  transaction whose publication was refused afresh once its window has run
  out, so the timer comes back a window later rather than in the past.

The crate performs no I/O, so it is tested on the host alone
(`cargo test -p tairix-writeback`).
