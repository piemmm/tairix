# `tairix-fat`

`lib/fat` is the FAT file system's on-disk format: FAT12, FAT16 and FAT32 as
Microsoft's FAT specification defines them, with the VFAT long-name
extension. The [FAT32 driver](../filesystem/fat32.md), the volume probe
(`lib/fsprobe`), the MBR writer's FAT partition types (`lib/partition`) and the
metadata registry's Atari attributes (`lib/fsmeta`) reach the format here
rather than each carrying a copy.

| Module | What it is |
|---|---|
| `boot` | The boot sector and its parameter block (`Bpb::parse`), every field's offset (`at`), and where the FATs, the fixed root and the data region lie. |
| `table` | Allocation-table entries (`Link`: free, next, end, bad) of 12, 16 or 28 bits, read and written through the bytes they span. |
| `dir` | Short entries (`ShortEntry`) and their field map, long-name entries, the checksum binding them to a short name, and a long name reassembled (`LongName`). |
| `name` | 8.3 names, the `~N` aliases a long name is given (`AliasStem`, `TailWindow`), and labels. |
| `stamp` | An entry's creation, access and write stamps, over `lib/abi`'s `DosTime`. |
| `fsinfo` | FAT32's free-space hint sector. |
| `format` | A new volume's parameters (`plan`), its boot sector, and what each sector of its system area holds. |

## The kind and the clusters

FAT32 is told by the parameter block's shape — no fixed root and no 16-bit
FAT size — as every reader tells it; any other block is FAT12 below 4,085
clusters and FAT16 below 65,525, and a block counting more is refused. The
clusters a volume has are those its sectors hold, as many as its table
describes and its kind counts, so no cluster lacks a table entry.
`Bpb::clusters_within` bounds them by the device a volume lies on too: a
reader uses a volume no further than its device goes.

## Names

A short name spells upper-case letters, digits, the format's punctuation and
code-page bytes. FAT records no code page, so an alias spells none: a long
name's other characters become `_`. An alias takes the lowest `~N` tail the
directory leaves free, which `TailWindow` finds over a thousand tails per walk
of the directory. A label spells upper-case letters, digits, spaces and the
format's punctuation — no code-page byte, for want of a code page — and is
never `NO NAME`, the placeholder formatters write for none.

## A new volume

`format::plan` lays a volume on a medium: FAT12 below 16 MiB, FAT16 to 2 GiB
and FAT32 beyond unless a kind is asked for; FAT16's cluster sizes from the
specification's table, FAT32's at the thresholds `mkfs.fat` applies, FAT12's
the smallest that keeps it under its count; a table as small as maps every
cluster after it. Whatever the request, the volume counts the clusters its
kind is typed by. Its system area is zeros but for the boot sector and the
sectors `format::marked_sectors` names, which `format::system_sector` fills; a
writer lays the boot sector last, so no reader takes a half-made volume for
one.

## Verification

Each module's tests hold it to hand-worked values; the fuzz target
(`tests/fuzz_fat.rs`) drives the boot sector, directory and table decoders
with well-formed volumes of every kind mutated and with noise, checking that
a parsed volume lays every region inside itself and writes back as itself,
that every short entry encodes back to its bytes, and that setting a table
entry leaves its neighbours alone.
