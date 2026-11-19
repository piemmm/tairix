# `tairix-filecore`

`lib/filecore` is RISC OS FileCore's disc format. The [ADFS
driver](../filesystem/adfs.md), the volume probe (`lib/fsprobe`) and the [TAIRiX
Bridge](../desktop/tairix-bridge.md)'s FileCore and RamFS reach FileCore
discs here rather than each carrying a copy. Every access goes through the
`Disc` trait, whose failures pass through as `Error::Disc`; what the format
refuses is a `Fault`, one for each condition RISC OS reports apart.

| Module | What it is |
|---|---|
| `record` | The disc record (`DiscRecord`), its zones (`fit_zones`), the boot block, the checksums, the least fragment and the whole sectors an object is allocated (`allocation_bits`). |
| `defect` | The boot block's defect lists (`Defects`): byte addresses below 512 MB, then, on a big disc, the sector numbers past it, each list ended by its check byte (`check_byte`). |
| `oldmap`, `newmap` | Allocation in the old map's free-space list and the new map's zones and fragments; a defect mapped out (`map_out`) and a new map compacted (`compact`). |
| `dir`, `bigdir` | The fixed and the big directories. |
| `format` | A new disc's layout (`new_map_record`, `lay_out`) for each `Variant`, the RAM disc's among them. |
| `identify` | Where a disc's structures lie (`Layout`). |
| `volume` | A disc's objects and directories (`Volume`), attached to a disc for one operation and detached between them (`Mount`). |

## Where objects go

What `volume` does with an object is what RISC OS 5's FileCore does, recorded
on the RAM disc:

- **Allocation** is in whole sectors, no fewer than a fragment takes: a
  2,100-byte file on a disc of 1K sectors takes 3K. A file is placed from its
  directory's zone on (`allocate_near`), anything else from the central zone
  on, as Acorn's E-format notes place them.
- **Sharing.** An object whose whole share granules come short of the least
  fragment shares one (`place_object`): room in a fragment the directory's
  objects share, else a new fragment of the least size from its start. The
  map's own fragment, which a fixed root shares behind the map, gives no
  room, nor does a fragment the directory shares from past its start. An
  empty object has no fragment and is addressed `EMPTY_OBJECT`.
- **Growing an open file** (`growth_base`, `grow_object`): past its
  allocation, it takes every byte the disc has free. One with a fragment of
  its own, or alone in a shared fragment from its start, grows within it; one
  sharing with others moves to a fragment of its own.
- **Settling a closed file** (`settle_object`): an empty one holds nothing; a
  small one moves into the directory's shared room, else keeps the least of
  its own fragment shared from its start; a larger one is cut to its extent;
  one already shared stays.
- **Moving** a file to another directory (`copy_for_move`): a file sharing its
  fragment with others takes a new fragment of its own, since FileCore shares
  a fragment within one directory only; a file alone in its fragment takes
  the fragment along.

- **Big directories** keep their name heap in entry order, a new name
  joining it where the entry it precedes keeps its own, and below the tail a
  word for each entry, in entry order, holding its disc address.

- **The disc's own structures are no object's.** An address only a broken
  directory could hold is refused before anything changes: on a new map one
  in the defects, or in the map's fragment anywhere but from the root on, in
  the piece the root begins (`NewMap::may_address_object`, `locate`); and no
  object grows, shrinks or frees the defects or the map's fragment. On an old
  map, one starting in the map or the root's sectors, the root's own start
  apart. A new map describing anything but a defect past the disc's end is
  refused when it is opened.

## What holds a byte, defects and compaction

- `holder_at` answers what holds a byte of the disc — free space, a defect, a
  new map's fragment or an old map's allocated sector — and `Holder::holds`
  whether an entry lies there, which `OS_FSControl` 52 searches with.
- `add_defect` records a defective sector as RISC OS 5 records one: in the
  boot block's first list, where the disc has one, kept ascending and
  unique, and mapped out of the map's free space. The mapped-out run is the
  sector widened to the least fragment in whole sectors, forward where the
  free space runs on and backward where it ends; it joins a defect it meets
  — the one covering the map past the disc's end among them — and takes any
  sliver of free space too small to stand alone either side. A sector an
  object or the disc's structures hold is refused (`DefectInUse`), one past
  the disc only listed. `defects` lists them: the boot block's lists, or, on
  a disc without one, the map's defect fragments sector by sector. An old map
  records none.
- `compact` gathers a new map's free space as RISC OS 5's `*Compact` does,
  zone by zone, moving only a fragment of one piece, which keeps its id, so
  no directory changes. Until a zone's free space is one run: the topmost
  file between the lowest free run and the last moves to the lowest run's
  start where it fits leaving no sliver; else the file just above that run
  slides down into it where it fits; else the fragment just above it, file or
  directory but not that topmost file, moves to the end of the last free run;
  else the zone is left. A directory never moves down. Every move goes to
  free space apart from what it leaves, so the data is copied before the map
  changes.

## Layouts

`format` lays out each disc FileCore makes, floppy and hard disc, as RISC OS
lays them out. The RAM disc's variant is RISC OS 5's RamFS's, bit for bit at
every size recorded: 128-byte map bits and 1K sectors, one zone up to 512K
with the map at the disc's start, and from 1M a hard disc's layout, its
record in the boot block and its map in the middle zone. A boot block opens
with empty defect lists, and a disc past 512 MB is marked big and keeps the
second. `lay_out` takes the root's name, and may leave the boot block out, as
`FileCore_LayoutStructure` lays a disc out.
