# FAT32 driver

`drivers/filesystem/fat32` (`tairix-drv-fs-fat32`) is the read/write
FAT32 driver. It is the first block-backed `drivers/filesystem/*`
crate, chosen first because FAT32 backs the EFI system partition and SD
cards (`AGENTS.md` §11).

It reads and writes a FAT32 volume sitting behind any
[`Block`](../abi/driver_traits.md#block) device and exposes it through
the versioned [`FilesystemRead`](../abi/driver_traits.md#filesystem) and
`FilesystemWrite` traits. The frozen `Filesystem` trait carries only
`mount`/`unmount` and a `DriverHandle` and cannot perform I/O, so each
I/O surface is a new versioned trait rather than a widening of the
shipped one (`AGENTS.md` §2.4 / §9).

## What the driver does

| Operation             | FAT32 mechanism                                   |
| --------------------- | ------------------------------------------------- |
| `open`                | Validate the boot sector (BPB) and compute layout.|
| `root`                | The root directory at the BPB's root cluster.     |
| `lookup`              | Scan a directory's entries for a name.            |
| `node_info`           | Read a file's short entry; walk a directory's chain. |
| `read_at`             | Walk the FAT cluster chain and copy file bytes.   |
| `read_dir`            | Hand a visitor a directory's entries in on-disk order, from a cursor. |
| `parent`              | Read a directory's `..` entry.                    |
| `create`              | Write a new LFN set + 8.3 entry; alloc a dir cluster for a directory. |
| `write_at`            | Extend/chain clusters, zero-fill gaps, write bytes, update the entry. |
| `truncate`            | Free the tail chain (shrink) or zero-extend (grow); update the entry. |
| `remove`              | Mark the entry + its LFN run deleted and free the chain, unless a pin holds the node. |
| `pin` / `unpin`       | Count the kernel's holds on a node; the last unpin of a removed one frees it. |
| `release_pins`        | Release every hold, freeing each removed node a pin kept. |
| `flush`               | No-op: every mutation writes straight through to the device. |

## Long file names (VFAT)

Each directory entry exposes a **single** name. When a valid long-name
set precedes the 8.3 short entry — its sequence is contiguous, its
`0x40` last-logical fragment is present, and its checksum matches the
short name — the driver reconstructs that long name; otherwise it uses
the 8.3 short name (so a volume written without long names stays fully
readable). When a long name is present the internal 8.3 alias is *not*
separately resolvable.

Names are returned as **UTF-8**: the long name's UTF-16LE code units
(including surrogate pairs) are decoded, and the driver falls back to
the short name on any malformed set — an unpaired surrogate, an invalid
scalar value, or a checksum mismatch — rather than surfacing a partial
name. A long name longer than the VFS's 255-byte name limit once decoded —
another system's name in a script of three-byte characters — is listed and
reached by its short alias, so it neither hides the file nor makes the
directory unlistable.

## Listing

A listing cursor is a **logical slot index** within the directory,
resolved by walking the directory's own cluster chain, so no cursor — stale
or forged — reaches another directory's clusters. Slots never move while in
use: a deletion marks its slots in place and an insertion takes a free run,
so an entry present throughout a listing is listed exactly once. A walk
reads each device block once, holding the block its last slot came from.

A corrupt chain fails closed rather than looping: a link past the volume's
last cluster is a bad chain, a chain walk stops at the volume's cluster
count, a directory walk at the format's 65,536 entries, and a move's walk up
`..` links at a link that comes round again or names no parent.

A listed entry's `allocated` is the clusters its size needs; only a
directory, whose entry records no size, has its chain
walked. A listing therefore never costs a file's size.

## Node ids

A file's own `NodeId` is the place of its short entry: the entry's device
byte offset over 32, with bit 63 set. A directory's is its first cluster, with
a directory flag and a validity flag set, so the two never collide and neither
is `NodeId::NONE`. A file's own id is also the number `stat` reports.

A file's entry holds its size, first cluster and stamps, and a write or a
truncation rewrites it where it lies, so the id lasts through them. A rename
writes the entry afresh at a new place and a removal frees it, so neither
keeps the id. Every operation by a file's own id reads the entry first and
answers `NotFound` for anything but a file's short entry in the data region —
a freed slot, a long-name fragment, a `.` entry, a directory's entry, a label
— so an id a removal or a rename left behind names nothing until a new entry
takes its slot. An id is good for a write only until the next name change in
its directory.

A directory's parent is the one its `..` entry names, and the root's is the
root. A directory whose first cluster is free, as a removed one's is,
answers `NotFound`, and takes no new entry.

## Pins

A file the kernel pins is reported under a token, bit 62 and a number the
mount never gives twice, by lookups and listings until its last unpin, while
`stat` goes on reporting its entry's place; a rename carries the token to the
file's new entry. A pin therefore changes the id a file is reported under,
and the driver declares `NodeIdentity::ContentDerived`. A pinned directory
keeps its own id: its first cluster is not freed while a pin holds it. Every
table a pin is recorded in grows fallibly, so a hold that cannot be recorded
is refused with `OutOfMemory`.

A removal of a pinned file, or a rename replacing it, deletes its entry but
keeps its chain: the pin keeps the file's first cluster, size and stamps, the
file reads, writes and truncates through its token, and `stat` reports no
name and the number its entry had. A removed directory a pin holds keeps its
cluster but takes no entries: a lookup in it, its parent and a creation or
move into it answer `NotFound`, and it lists nothing. The last unpin frees
either (`Unpinned::Freed`), and `release_pins` frees every one.

No pin outlives the mount. FAT keeps no orphan record, and planting one would
show other systems an entry or a field they do not expect, so a crash with a
deleted file still pinned leaves its chain allocated and named by no entry: a
lost chain for the format's own checker to reclaim.

## FAT type detection

The driver identifies FAT32 by the boot-sector shape — a zero 16-bit
FAT size and a zero root-entry count — rather than by re-deriving the
cluster-count threshold. A FAT12/FAT16 volume has non-zero values in
those fields and is rejected with `DriverError::BadMagic`, so the
distinction is exact for the volumes this driver accepts.

The device logical-block size and the FAT bytes-per-sector are
decoupled: the driver addresses the volume in bytes through
[`tairix-bytedev`](../lib/bytedev.md), so the two sizes need not match.

## Writing

Writes and truncations take the file's id, its entry's place or its pin's
token; create, remove and rename take a `(dir, name)` pair. Each created entry is written
as a VFAT long-name set bound to an 8.3 short alias taking the lowest `~N`
tail no live alias holds, so an arbitrary, case-preserving name round-trips
through a later
read. One walk of the directory chooses both the tail and the run of free
slots the set takes. Free clusters are found by scanning the FAT;
directories grow by one zeroed cluster at a time when their entry slots
are exhausted, up to the format's 65,536 entries, past which a create is
refused as out of space; and every FAT
mutation is mirrored across all FAT copies. Sub-block writes are
read-modified-written so neighbouring bytes are preserved. A rename writes the
entry afresh under its new name and alias, carrying its attribute byte and its
stored stamps verbatim, then frees the old slots.

## Timestamps

A create stamps the new entry with the host's wall clock
(`VolumeHost::wall_time`, [driver traits](../abi/driver_traits.md)): its
creation date, time and hundredths, its last-write date and time, and its
access date. A new directory's `.` and `..` take the same stamps. A write or
truncate moves the last-write stamp in the same write that updates the
entry's cluster and size, and a rename carries every stamp across. A read
moves nothing, so the access date stays the creation date. A directory's
own entry keeps the stamps it was created with: FAT reaches that entry only
through the parent, so finding it whenever the directory's contents change
would cost a scan of the parent per name added or removed.

FAT records no zone and TAIRiX stores none (`plans/TIMEZONES.md`), so the
fields hold UTC civil time and are read back as UTC. A volume another system
wrote in local time reads shifted by that zone's offset. The fields hold
1980 to 2107: the last-write time to two seconds, the creation time to the
hundredth. A time outside that range, or no time while the clock is unset,
is written as the zero date FAT reads as none, which reads back as
`Time64::UNIX_EPOCH`. FAT keeps no metadata-change stamp, so `changed` is
always that.

A listing carries every entry's stamps, and a stat by node reports the
same: a file's from the entry its id locates, a directory's from its entry
in the parent its `..` names. The root, which no entry names, has none, nor
has a directory whose `..` is damaged or names a parent not holding it; such
a directory still stats.

## End-to-end QEMU vertical

`tests/integration/fat32_virtio_blk_pci_x86_64` exercises the driver
against a **real (emulated) virtio-blk-pci device** under QEMU. It boots
the production kernel pipeline, brings the block device online through
the same shared bring-up the virtio-blk vertical uses, then mounts a
planted FAT32 volume through `Fat32::open`, verifies the planted file
reads back its known contents, and creates + writes + reads back a fresh
file before signalling success.

The on-disk image is built by the shared `tairix-test-fat32-image`
fixture (a 1 MiB volume, two mirrored FATs, one-sector clusters). The
host harness (`cargo xtask test --qemu`) plants exactly that image on the
backing disk, and the freestanding guest tail names the same planted and
to-be-written files through the fixture's constants, so the two sides
share one source of truth (`AGENTS.md` §2.2). The device tail
(`fat32_round_trip`) is generic over the virtio transport, so a riscv64
MMIO sibling runs identical code.

## Permissions

FAT32 stores no owner, mode, ACL, or capability gate. Those live in the
VFS metadata layer ([Permissions](./permissions.md), `AGENTS.md` §5.3);
the driver makes no permission decisions (§5.4 — the VFS is the policy
point, the driver is raw structural I/O). Every node reports one fixed
record, system-owned with `0755` for a directory and `0644` for a file; a
volume attached as removable media is mounted under the storage-group
identity map instead. A create ignores the record it is given, which FAT has
nowhere to keep, so a new node is owned as every node is, and an explicit
mode or owner change is refused as one the format cannot hold
(`NotSupported`). Names match ignoring ASCII case, as
the format requires, and the driver declares that rule
(`NameMatching::AsciiCaseInsensitive`), so the VFS applies it to a no-replace
rename: `B.TXT` beside `b.txt` is a clash, and a rename to a case variant of an
entry's own name re-spells the entry. No Unicode normalisation is applied.

## Limitations

- The driver writes through to the block device synchronously; there is
  no in-memory write-back cache or journal (FAT has no journal). A
  generated short alias uses a `~N` numeric tail rather than the legacy
  6-character-plus-hash scheme; collisions are still resolved against the
  live directory, so on-disk uniqueness holds.

## Extended-metadata representability

FAT32 has no per-file extended-attribute store and the driver does not
implement the `FilesystemAttrs` ABI (its attribute facet answers `None`, so
the `fs_attr_*` syscalls refuse a FAT32 mount with `NotSupported`), so it
cannot represent ARXFS extended
attributes or foreign-filesystem preset metadata (`arxfs-spec.md` §21). Under
the cross-filesystem preservation contract, an exact-preservation copy of a
file carrying such attributes onto FAT32 reports `MetadataNotRepresentable`; a
best-effort copy drops them only under an explicit, documented lossy policy.
Its own timestamp range, precision and zone are those [Timestamps](#timestamps)
gives; no interface reports a format's timestamp limits to a caller
(`plans/OPEN-DEFECTS.md` D1048).
