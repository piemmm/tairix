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
is a partition table, not a volume, and is not opened (DI10). A volume whose
root says its bitmap is not validated opens read-only (`NeedsCheck`), as
AmigaDOS would revalidate it.

## Damage

Every block is validated as it is read — a header's type, self-pointer,
secondary type, name and comment bounds, pointers within the volume, and
checksum; an extension block's owner; an OFS data block's owner, sequence
number and byte count; a bitmap block's checksum — and a mismatch is
`BadMagic`. Every chain (hash chains, extension chains, links, the bitmap's
extensions, a directory's ancestry) is walked under a bound with Brent's
loop check, so a damaged volume cannot hold the driver.

## Names and links

Names are ISO 8859-1 on disk and UTF-8 above, one to 30 bytes, with `:`,
`/` and control characters refused. A plain volume matches names
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
bitmap is a refusal and writes nothing. One that fails after, or finds a
structure damaged, leaves the bitmap invalid and refuses further writes,
though a later flush still forces the device's cache. A file's header
commits its size: a growth writes everything else first and a cut writes the
header first, and a read takes a block describing more than the size only as
far as the size, so a change cut short at any write reads back the file as it
was or as it became. A removed object the kernel still holds keeps its blocks
until its last unpin, and while one does the root is left marked invalid, so
a crash before that unpin is revalidated and its blocks reclaimed.

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
