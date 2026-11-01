# AmigaDOS driver

`drivers/filesystem/affs` (`tairix-drv-fs-affs`) reads and writes AmigaDOS
volumes — 880 KiB and 1760 KiB floppies, the `.adf` images of them, and
bare hard-disk partitions (`.hdf`) of any block size the format allows —
behind any `tairix_abi::driver::block::Block` device, through the versioned
`FilesystemRead`, `FilesystemWrite`, `FilesystemAttrs` and
`FilesystemStats` traits. Its `Run` binary serves a volume from user space
([served volumes](./served.md)), bound by `tairix,volume-affs`; a disk image
reaches it through [the image attach](./images.md).

## Formats

| Type | Filesystem | Read | Write |
|------|------------|------|-------|
| `DOS\0` | OFS | ✓ | ✓ |
| `DOS\1` | FFS | ✓ | ✓ |
| `DOS\2` | OFS, international | ✓ | ✓ |
| `DOS\3` | FFS, international | ✓ | ✓ |
| `DOS\4` | OFS, directory cache | ✓ | read-only |
| `DOS\5` | FFS, directory cache | ✓ | read-only |
| `DOS\6`, `DOS\7` | long filenames | refused | — |

Recognition is `lib/fsprobe`'s (`tairix_fsprobe::affs`): the boot block's
`DOS` type and a root block — for any block size from 512 to 32768 bytes —
whose type, secondary type, hash-table size and checksum all hold. A
directory-cache volume reads through its hash chains, which its caches only
summarise; writing it would leave the caches stale (`plans/DISK-IMAGES.md`
DI8), so it opens read-only (`UnsupportedFeature`). Long-filename volumes
are refused (`Unsupported`, DI9). A disk partitioned by a Rigid Disk Block
is a partition table, not a volume, and is not opened (DI10 for an image,
DI20 for a device). A volume whose
root says its bitmap is not validated opens read-only (`NeedsCheck`), as
AmigaDOS would revalidate it.

## Damage

Every block is validated as it is read — a header's type, self-pointer,
secondary type, name and comment bounds, pointers within the volume, and
checksum; an extension block's owner; an OFS data block's owner, sequence
number and byte count; a bitmap block's checksum — and a mismatch is
`BadMagic`. Every chain (hash chains, extension chains, links, the bitmap's
extensions, a directory's ancestry) is walked under a bound with Brent's
loop check, so a damaged volume cannot hold the driver. A change that frees
or fills what a file's extension chain names first walks the chain on to the
block that ends it: one that returns to a block it passed never ends, so it
is damage found before anything is written, and nothing a cut frees is also
kept. The root and the
bitmap's own blocks are named once each, every bitmap block the volume needs
is named, and none is ever written or freed as anything else; a bitmap that
names a live object as one of its pages, or marks one free, is found only by
validating the tree.

## Names and links

Names are ISO 8859-1 on disk and UTF-8 above, one to 30 bytes. A name a new
entry, a link or a rename is given refuses `:`, `/` and control characters;
an entry another writer recorded under one is still found, read, renamed and
removed by it. A plain volume matches names
ASCII-case-insensitively, an international one Latin-1-case-insensitively —
the fold its directory hash is computed with.

A hard link to a file is that file under a second name: one node, its link
count the chain's length; removing the file's own name promotes its newest
link, as AmigaDOS does. A soft link reads in POSIX spelling (AmigaDOS's
leading `/` is `..`, `:path` on this volume is `/path`); a hard link to a
directory reads as a soft link to the directory, so the tree stays a tree.

## Writing

A change's first write opens its transaction: the root, its bitmap marked
invalid, is written and the device flushed before anything else. The commit,
which `lib/writeback` paces, writes the bitmap back and flushes, and only
then marks the root valid; a crash in between leaves a volume AmigaDOS
revalidates. A change that fails before it writes or moves a block in the
bitmap is a refusal and writes nothing, and a heap running short is one: the
buffers a change takes once it has written are held before it, and the
commit reads and allocates nothing. One that fails after, or finds a
structure damaged, leaves the bitmap invalid and refuses further writes,
though a later flush still forces the device's cache. A file's header
commits its size: a growth writes everything else first and a cut writes the
header first, and a read takes a block describing more than the size only as
far as the size, so a change cut short at any write reads back the file as it
was or as it became. The extension chains of the few files most recently
reached are kept, every growth and cut keeping them in step, so no read or
write walks one from its header again; a cut checks all it frees before it
writes and frees it without listing it, and the zeros a growth lays down move
in runs. A listing keeps the bucket a call stopped in until the next change,
so one resumed an entry at a time, after the entry it stopped on or at it,
reads each entry once. A removed object the
kernel still holds keeps its blocks until its last unpin, and while one does —
or a free at that unpin failed — the root is left marked invalid, so a crash
before that unpin is revalidated and its blocks reclaimed. A free the heap
refuses at that unpin stays owed, and is made when the mount's pins are
released.

## Metadata

The protection bits' low byte and the comment are `amiga.protection` and
`amiga.comment`, through `tairix_fsmeta`'s Amiga preset
([metadata registry](./metadata-registry.md)). An object's one date stamp —
days since 1978, minutes, ticks — is all four of its times, read and written
as UTC, as FAT's are. AmigaDOS records no owner, mode or ACL: the VFS
presents the volume and makes every permission decision.

## Tests

`cargo test -p tairix-drv-fs-affs` formats OFS and FFS, plain and
international, at several block sizes, and checks every image against an
independent walker of the raw blocks; a randomized model test drives
hundreds of changes and reopens the result, and `tests/fuzz_affs_mount.rs`
mutates populated images and walks, reads and changes whatever opens.
