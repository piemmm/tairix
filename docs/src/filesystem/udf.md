# UDF driver

`drivers/filesystem/udf` (`tairix-drv-fs-udf`) mounts OSTA UDF 1.02 to 2.60
volumes behind any `tairix_abi::driver::block::Block` device, through
`FilesystemRead`, `FilesystemWrite`, `FilesystemSecurity`, `FilesystemAttrs`
and `FilesystemStats`, and formats new ones (`Udf::format`). Recognition —
the volume recognition sequence, the anchor, the prevailing descriptors and
the volume's identity and label — is shared with the volume probe
(`lib/fsprobe`'s `udf` module), as are the descriptor tags and their CRC.

## What is read

Every partition kind UDF defines:

- **physical** partitions, on any medium;
- **sparable** partitions, a rewritable disc's remapped packets read through
  their sparing table;
- **virtual** partitions of write-once media, read through the virtual
  allocation table found at the last recorded block the drive reports
  (`Block::disc_layout`), in both the 1.50 and the 2.00 layouts;
- **metadata** partitions (UDF 2.50 and later), the file entries read through
  the metadata file or its mirror.

A node's id is the address of its ICB, which writes, truncations and renames
leave where it lies, so the id lasts as long as the node
(`NodeIdentity::Stable`); it is also the number `stat` reports. Writes,
truncations and punched holes take the file's id, and the name operations
take a `(dir, name)` pair. A directory's parent is the one its parent
descriptor names, the root naming itself.

File entries and extended file entries, embedded data, short and long
allocation descriptors, allocation extent descriptor chains and unrecorded
extents (which read as holes) are followed, each chain and indirection
bounded so a hostile disc cannot loop it. Hard links share a node and its
link count; symbolic links read back as their path; names are OSTA
Compressed Unicode, matched exactly. A name for a node of a kind this system
has none for — a device, a pipe or a socket another system made — is not
listed, looked up, removed or renamed, and a rename over it is refused, since
nothing could judge its occupant; the name stays taken.

## What is written

A volume is written in place when it has one physical partition of
rewritable or overwritable media with a space bitmap, records a revision
from 1.02 to 2.60, was closed cleanly, and is not write-protected.
`Udf::write_refusal` names why any other is mounted read-only:
`write_protected`, `revision_unwritten`, `not_closed`,
`media_not_rewritable`, `no_space_bitmap`, `sparable_media`,
`write_once_media`, `metadata_partition`, `several_partitions` — and
`damaged`, below.

Writing creates, extends, truncates, punches holes in, renames and removes
files, directories and links, and records owners, groups and modes. A new
node is born with the owner, group and mode of the record it is created with,
kept in its file entry's owner, group, permission word and ICB flags. A file
entry has no place for a record's ACL entries or capability gate: a new node
is born without them, and `set_security` refuses a record carrying either, or
a POSIX ACL, rather than store part of it. A change that runs out of space
part-way leaves the volume as it was: blocks are taken from the bitmap before
anything that refers to them is rewritten, and given back on failure. A
node's content — a directory's parent descriptor, a link's path — is
recorded before its name is added, so a node that could not be filled is
never named. A rename over an existing name points that name at the node
where it lies, and a removal or rename rewrites descriptors in place, an
embedded directory's inside its entry, so neither needs space. Freeing a
node releases its entry block but leaves its bytes, which would still read
as the node, so once the space bitmap is open every operation by an id whose
entry block it holds free answers `NotFound`, until the block is taken
again.

The integrity descriptor is marked open before a change's first write and
closed when the change is published: its space bitmap written back and the
descriptor rewritten. A mounted volume is published within the write-back
window `lib/writeback` sets by the device's command cost, the host's timer
firing for a volume that falls quiet; without a host, or with a host whose
clock does not answer, every change is published as it ends. A change that
fails after writing for any reason but space may be part-made, so the volume
is marked `damaged`: writes are refused from then on, and the descriptor is
left open, so the volume mounts read-only until it is checked. A node's
extended attribute file and named streams are freed with it.

`Udf::format` lays out the recognition sequence, anchors at sector 256 and
at the last sector, a main and a reserve descriptor sequence, the integrity
descriptor, and one overwritable partition with a space bitmap, a file set
and an empty root, recording any revision from 1.02 to 2.01.

## Held nodes

`pin` holds a node for the kernel: it checks the node is live and answers
the node's own id. Holds are counted and `unpin` releases one; a hold the heap
cannot record is refused as `OutOfMemory`. A held node's entry block is never
freed, so its id cannot pass to another node. A held node that loses its last
name — removed, or replaced by a rename — keeps its entry and extents, its
link count recorded as 0, and the integrity descriptor counts it no more.
Through its id it still reads and writes, truncates, and takes changes to its
owner, mode and attributes, `node_info` counting no name, but it gains no name
again; a directory so removed takes no entries, lists nothing and has no
parent. Its last `unpin` frees it (`Unpinned::Freed`), and `release_pins`
frees every such node at once. UDF keeps no orphan record, so a crash before
that last unpin leaves the node's blocks allocated, with nothing naming them,
until the volume is checked.

## Foreign metadata

UDF keeps a Macintosh file's Finder information in an implementation-use
extended attribute (`*UDF Mac FinderInfo`), in the entry's own attribute
space or in its extended attribute file. It surfaces as `mac.type`,
`mac.creator` and `mac.finderflags` ([metadata registry](./metadata-registry.md)),
read and written; a directory keeps only its Finder flags. UDF has nowhere
to keep any other attribute, so setting one is refused as unsupported — the
refusal the copy engine reports as an attribute the destination cannot
store.

A file's resource fork is a UDF named stream; streams are freed with their
file but not yet read or written as `mac.resourcefork`
(`plans/FOREIGN-MEDIA.md`).

## Served from user space

The driver is served from user space ([Volumes served from user space](./served.md)): its `Run` binary binds
`tairix,volume-udf` and runs with `CAP_FS_SERVE` and `CAP_LOG_EMIT`. A volume
it will not write — a write-once medium, a revision past its own — is offered
read-only with its reason, which the kernel records as it mounts it so.

## Bounds

ICB indirections are followed at most 64 deep, the file set descriptor
sequence at most 16 blocks, an extended attribute file at most 1 MiB, a
sparing table at most 4096 entries — read no larger than that, whatever size
its map declares — and the virtual allocation table is searched at most 32
blocks back from the last recorded one, past any candidate that cannot be
read. A virtual partition sits only on the physical or sparable map of its
number, and a metadata partition on the virtual or physical one of its
number, so no chain of maps is longer than two. A file records at most
2^20 extents, a hole longer than one descriptor holds is split across
several, and a size whose holes would need more is refused. Every block a
written file names lies in its partition, every directory descriptor's tag
names the block it lies in, and its CRC covers it through its name. The fuzz harness (`tests/fuzz_udf_mount.rs`) mutates
formatted, populated volumes and mounts, walks and writes them.
