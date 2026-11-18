# tairix-filecore

RISC OS FileCore's disc format: the disc record and boot block, the old map
and the new map's zones and fragments, the fixed and big directories, a new
disc's layout, and a disc's objects and directories over them. The ADFS
driver, the volume probe and TAIRiX Bridge's FileCore all read and
write FileCore discs through it, so the format has one definition.

- `record` — the disc record, its geometry and zones, the boot block and the
  checksums; the least fragment and the whole sectors an object is
  allocated.
- `oldmap`, `newmap` — allocation in each map.
- `dir`, `bigdir` — the fixed and the big directories.
- `format` — a new disc's layout, the RAM disc's among them as RISC OS 5's
  RamFS lays one out.
- `identify` — where a disc's structures lie.
- `volume` — a disc's objects and directories, and where objects go: an
  object small enough shares a fragment, an open one grows as far as the
  disc's free space reaches and is settled when it closes, and a file moved
  out of a fragment it shares takes a fragment of its own, each as RISC OS 5
  does them.

`no_std`, with no `unsafe`. Every decoder is total: a structure that breaks
the format is refused as a `Fault`, never trusted. The ADFS driver's mount
harness and the volume probe's fuzz the decoders, and TAIRiX Bridge's
RAM disc harness the allocation over a disc scribbled on.

## Stability

Tier: `experimental`. The format is fixed; the Rust API may change while
TAIRiX is pre-release.
