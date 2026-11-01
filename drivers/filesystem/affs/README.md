# `tairix-drv-fs-affs` — AmigaDOS filesystem driver (OFS and FFS)

Reads and writes AmigaDOS volumes — 880 KiB and 1760 KiB floppies, the
`.adf` images of them, and bare hard-disk partitions (`.hdf`) of any block
size the format allows — behind any `tairix_abi::driver::block::Block`
device, through the versioned `FilesystemRead`, `FilesystemWrite`,
`FilesystemAttrs` and `FilesystemStats` traits.

Stability: `experimental`.

## Supported volumes

| Type | Filesystem | Read | Write |
|------|------------|------|-------|
| `DOS\0` | OFS | ✓ | ✓ |
| `DOS\1` | FFS | ✓ | ✓ |
| `DOS\2` | OFS, international | ✓ | ✓ |
| `DOS\3` | FFS, international | ✓ | ✓ |
| `DOS\4` | OFS, directory cache | ✓ | read-only (`UnsupportedFeature`) |
| `DOS\5` | FFS, directory cache | ✓ | read-only (`UnsupportedFeature`) |
| `DOS\6`, `DOS\7` | long filenames (AmigaOS 3.2) | refused (`Unsupported`) | — |

Recognition is `lib/fsprobe`'s (`tairix_fsprobe::affs`): the boot block's
`DOS` type and a root block at the volume's middle — for any block size from
512 to 32768 bytes, or where the boot block says — whose type, secondary
type, hash-table size and checksum all hold. A directory-cache volume reads
through its hash chains, which are what its caches summarise, so it reads
whole; writing it would leave the caches stale (`plans/DISK-IMAGES.md` DI8).
An Amiga disk partitioned by a Rigid Disk Block is a partition table, not a
volume, and is not opened (`plans/DISK-IMAGES.md` DI10 for an image, DI20
for a device).

A volume whose root says its bitmap is not validated — one a system stopped
writing before it finished — opens read-only (`NeedsCheck`): AmigaDOS
rebuilds such a bitmap when it validates the volume.

## Structure and damage

Every block is validated as it is read: a header's type, self-pointer,
secondary type, name and comment bounds, pointers within the volume and
checksum; an extension block's owner and pointer count; an OFS data block's
owner, sequence number and byte count; a bitmap block's checksum. A mismatch
is `BadMagic`, never a guess. Every chain — hash chains, extension chains,
links, the bitmap's extensions, a directory's ancestry — is walked under a
bound with Brent's loop check, so a damaged volume cannot hold the driver.
A change that frees or fills what a file's extension chain names first walks
the chain on to the block that ends it: a chain that returns to a block it
passed never ends, so it is damage found before anything is written, and
nothing a cut frees is also kept. A data block two of a file's tables name,
or another file's tables name too, is found only by validating the tree
(`plans/DISK-IMAGES.md` DI11).
The root and the bitmap's own blocks are named once each, every bitmap block
the volume needs is named, and none is ever written or freed as anything
else. A bitmap that names a live object as one of its pages, or marks one
free, is lying in a way only validating the tree finds.

## Names and links

Names are ISO 8859-1 on disk and UTF-8 above, one to 30 bytes. A name a
change gives an entry (a create's, a link's, a rename's) refuses `:`, `/` and
control characters; a lookup, a removal and a rename's source find an entry
by any name the volume records. A plain volume folds ASCII
(`NameMatching::AsciiCaseInsensitive`); an international one folds Latin-1
letters too (`NameMatching::Latin1CaseInsensitive`), the same fold the
directory hash is computed with.

* A hard link to a file is that file under a second name: one node id, its
  `nlink` counting the chain of links. Removing the file's own name promotes
  its newest link to take its name and place, as AmigaDOS does; the file
  keeps its id, data and other links.
* A soft link reads in POSIX spelling: AmigaDOS's leading `/` (parent) is
  `..`, `a//b` is `a/../b`, and `:path` or `Volume:path` on this volume is
  `/path`, resolving within the projection that shows it. A target on
  another volume or an assign keeps its stored spelling. `create_link`
  stores the inverse; a target naming a volume (`:`) is refused.
* A hard link to a directory reads as a soft link to the directory's path
  within the volume, so the tree stays a tree.

## Writing

A change's first write opens its transaction: the root, its bitmap marked
invalid, is written and the device flushed before any other block. The
bitmap, changed in memory, is written back at the commit, which
`lib/writeback`'s `CommitScheduler` paces (eager with no host); the device
is flushed, and only then is the root marked valid. A crash in between
leaves a volume AmigaDOS revalidates. A change that fails before it writes a
block or moves one in the bitmap is a refusal and writes nothing, and a heap
running short is one: the block buffers a change takes once it has written
are held before it, a walk handing each step's back, the bitmap's page
buffers are taken as it loads, before any write, and the commit reads and
allocates nothing. One that
fails after, or that finds a structure damaged, leaves the bitmap invalid and
refuses further writes (`Damaged`); its in-memory bitmap is dropped, and the
next flush still forces the device's cache. A new extension block is written
before the one that chains it, and hash chains are kept in ascending block
order. A file's header commits its size: a growth writes its data and
extension blocks before the header, and a cut writes the shorter header
before trimming its blocks, while a read takes an extension or OFS block
holding more than the size only as far as the size — so a change cut short
at any write reads back the file as it was or as it became. A freed header
is wiped, so an id that named it names nothing. A file grows from the block
after its last, and a listing counts each hard-linked file's names once
between changes.

A removed object the kernel holds — a file, a directory, a link to a
directory — keeps its blocks until its last unpin, and while one does — or a
free at that unpin failed — the root stays marked invalid, so AmigaDOS reclaims
them. A free the heap refuses at that unpin stays owed, and is made when the
mount's pins are released.

A file's extension chain — the blocks walked or linked — is kept for the
eight files most recently reached, and every growth and cut keeps it in step,
so no read or write walks a chain from its header again: a sequential read
reads each extension block once, and a call after a change reads only the
extension block it starts in. A read also keeps the file's header and current
extension block. FFS data runs that are contiguous on disk move in one
transfer, as do the zeros a growth lays down. A cut checks everything it
frees before its first write and reads each extension block it frees again
to give back what that names, so it holds no list of what it frees. A listing
keeps the bucket a call stopped in until the next change, so a listing
resumed an entry at a time, after the entry it stopped on or at it, reads
each entry once, and it resolves only the
entries it hands over. An allocation on a volume found full is refused
without searching the bitmap again.

## Metadata

The protection bits' low byte (raw: the owner's `rwed` deny when set) and
the comment surface as `amiga.protection` and `amiga.comment` through
`tairix_fsmeta`'s Amiga preset, the same reading the ISO 9660 driver gives
Rock Ridge's Amiga records. The one date stamp an object keeps — days since
1978, minutes, ticks — is all four of its times; AmigaDOS records no zone,
so it is read and written as UTC civil time, as FAT's is. A stamp is written
to the tick from the host's wall clock, or as zero when the time is unknown,
and a zero stamp reads as the epoch. The root reports its creation date as
`created`.

## Security

AmigaDOS records no owner, mode or ACL; the VFS presents the volume and
makes every permission decision. A create ignores the record it is given.
`#![forbid(unsafe_code)]` holds for the whole crate.

## Required capabilities

* `CAP_DRV_LOAD` at `register` time.
* The `Run` binary, built with the `program` feature, serves a volume
  through `lib/fsserve` (`Served`, binding `tairix,volume-affs`), with
  `CAP_FS_SERVE` and `CAP_LOG_EMIT` (`docs/src/filesystem/served.md`).

## Test surface

`cargo test -p tairix-drv-fs-affs` formats OFS and FFS, plain and
international, at 512, 1024 and 4096-byte blocks and on a volume past the
root's 25 bitmap pointers, and checks every image the tests leave against an
independent walker of the raw blocks that proves the tree and the bitmap
agree. It covers files across every block and extension boundary,
overwrites, appends, sparse writes and truncates against a model, tails
zeroed on regrowth, removal reclaiming space, name validation and both
folds, entries recorded under names no new entry may take, listings resumed
after every entry and across changes, renames and their refusals, the
transaction's bitmap protocol with and without a host, unvalidated,
directory-cache and long-filename volumes, hard links and
promotion, soft and directory links, pins of files and directories,
attributes, date stamps, a full volume, and damaged headers, data blocks,
chains and bitmaps, removing a hard link filed after its file or in the
directory it names, held directory links, the commit's write and flush order,
extension blocks written before they are linked, refused changes writing
nothing, and what sequential reads and listings read. Every kind of change,
on an eager and a deferred transaction and across a long file, meets a heap
that refuses each of its allocations in turn, and is refused before it writes
or lands whole; a commit and a walk made once a change has written take
nothing from the heap, and an orphan whose free meets a shortage is freed when
the pins are released. It measures what
appends, overwrites, two files read in turn and a copy within the volume read
of the extension chains, what a cut holds, what a growth over a gap writes,
what a listing resumed an entry at a time reads, and what an allocation on a
full volume reads; a chain that returns to a block it passed is refused to a
cut, a growth and a removal, and a damaged soft link fails only the listing
call that reaches it. Growths, zero growths and cuts are replayed cut short
after every one of their writes, each state reopened and read; a write
failing part-way, a read failing before any write, and a damaged volume
counting its space are each driven through a faulting device. Randomized
model tests drive hundreds of changes — one across more long files than the
chains kept, read cold beside it — and reopen the result.
`tests/fuzz_affs_mount.rs` mutates populated OFS and FFS images and a volume
past the root's bitmap pointers — byte flips, and sealed mutations that shape
a metadata block's pointers, counts, kinds and lengths and re-seal its
checksum, a bitmap extension block's unsealed — and walks, reads and changes
whatever opens: two files read in turn, a long file cut and grown again, each
directory listed again an entry a call and some reads made again, both
required to agree with the first answer where each is whole.
