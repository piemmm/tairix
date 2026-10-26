# ext4 driver

`drivers/filesystem/ext4` (`tairix-drv-fs-ext4`) reads and writes ext2,
ext3 and ext4 volumes behind any [`Block`](../abi/driver_traits.md)
device, through the versioned `FilesystemRead`, `FilesystemWrite`,
`FilesystemSecurity` and `FilesystemStats` traits. ext4 is the format many removable drives carry, so
a volume `mke2fs` made is written journaled where it has a journal, with every
checksum it carries maintained, and clean to `e2fsck` afterwards.

## On-disk support

| Feature | Read | Write |
| --- | --- | --- |
| ext2, ext3, ext4 layouts; block sizes 1, 2 and 4 KiB | yes | yes |
| 128-byte and larger inodes; 32- and 64-byte descriptors (`64bit`) | yes | yes |
| Extent trees of any depth; the classic map to triple indirect | yes | yes |
| Unwritten extents (read as zeros, written by converting them) | yes | yes |
| Linear and hash-indexed directories (`dir_index`, `largedir`) | yes | yes |
| `flex_bg`, `meta_bg`, `sparse_super`, `sparse_super2`, lazily initialised groups | yes | yes |
| `metadata_csum`, `metadata_csum_seed`, `gdt_csum` | yes | yes |
| `huge_file`, `large_file`, `dir_nlink`, `extra_isize` | yes | yes |
| The internal journal, and replay of one a crash left | yes | yes |
| The orphan chain and the orphan file | yes | yes |
| POSIX access ACLs | yes | as a mode change moves them |
| `inline_data` | files and links; inline directories refused | no |
| `bigalloc`, `mmp`, `ea_inode`, `encrypt`, `casefold` | yes, but encrypted names are refused and casefolded ones matched exactly | no |

`Ext4::open` validates the superblock (magic, geometry against the device,
the inode count against the groups, its checksum under `metadata_csum`)
and refuses an incompatible feature the read path does not implement, an
external journal, and a block size above 4 KiB. A volume the write path
cannot maintain — a feature it does not keep, errors a checker recorded, a
Hurd layout, a journal it cannot write or one a kernel aborted, an
unjournaled volume a crash left unclean, orphans that cannot be finished —
opens and serves reads, and `Ext4::write_refusal` names the reason. The
volume service then mounts it read-only and audits that cause (event
4180), so a refused write reads as a read-only file system.
`Ext4::open_read_only` never writes, and refuses a journal that needs
replay rather than serve the state beneath it. A replay can rewrite the
superblock and the descriptors themselves, so the volume is read again from
what it left, and that reading refuses a log still needing replay rather than
replaying it twice. A volume under multi-mount protection (`mmp`), which
another host may be writing, is never replayed, a replay being a write. A
writable mount sets the superblock's free counts from the group descriptors,
which every allocation keeps exact and a crash can leave the totals behind.

## Generations

A volume's features decide its generation, as `mount` names it: ext4 once any
incompatible or read-only-compatible feature past ext3's is on (ext3 knows
`filetype`, `recover` and `meta_bg`; `sparse_super`, `large_file` and
`btree_dir`), else ext3 with a journal and ext2 without. A compatible feature
never decides it, since an older driver mounts the volume regardless.
`Ext4::generation` reports it, and the volume service publishes it as the
mount's type, so `df`, `stat` and the file manager show an ext2 stick as ext2.

Writing never moves a volume to a later generation, so the drivers it was made
for still mount it:

- a file created on a volume without `extent` is block-mapped;
- a descriptor field only ext4 defines — its flags, bitmap checksums,
  unused-inode count and own checksum — is written only under the feature
  that defines it;
- the high half of an attribute-block number is read only under `64bit`: on
  ext2 and ext3 those bytes are padding, and GNU Hurd keeps its high mode
  bits there;
- a journal keeps the tag format it was made with, taking 64-bit tags and v3
  checksums only where the volume's own features ask for them.

A revision-0 superblock has no feature words and defines nothing past
`s_rev_level`. Before the driver stores anything only the dynamic revision
has — `large_file` for a file past 2 GiB, or the head of the orphan chain — it
raises the volume to revision 1, keeping the first inode and inode size
revision 0 fixes.

## Nodes and mapping

A `NodeId` is the on-disk inode number, so there is no in-memory inode
table; each operation reads the inode on demand. A byte write takes the
file's `NodeId`, and `parent` reads a directory's `..`. A freed inode keeps
its type, so its link count is what tells it from a live one: every call
answers `NotFound` for an inode no link and no hold reaches, and a stale id
never reads or writes a freed inode.

`pin` holds an inode for the kernel under its number, the id every lookup and
listing answers, and holds count. A held inode that loses its last link —
unlinked, or replaced by a rename — is listed on the orphan chain in that same
journalled operation and kept: it reports `nlink` 0 and takes reads, writes,
truncates and record changes until its last `unpin` releases it (blocks, inode
and chain entry, `Unpinned::Freed`). `release_pins` drops every hold and
releases each such inode. A removed directory a hold keeps lists nothing and
answers `NotFound` to `lookup`, `parent`, `create` and a rename into it. No
hold outlives the mount, so after a crash the chain alone remains and the next
writable mount releases what it lists, as `e2fsck` would.

Logical-to-physical block mapping covers both on-disk layouts:

- **Extent-mapped** inodes walk the extent tree from the inline root in
  `i_block`, descending interior index nodes up to the format's depth of
  five. An unwritten extent and a missing extent both read as zeros.
- **Block-mapped** inodes resolve the 12 direct pointers and the single,
  double, and triple indirect blocks. A zero pointer is a sparse hole
  and reads as zeros.

A read is one device request per physically contiguous run. Every
extent and indirect block is checked against its checksum and the
volume's bounds before it is followed.

## Symbolic links

A link's target is read with `read_link` in the spellings ext4 uses: the
**fast** form keeps it in the 60-byte `i_block` array and allocates no data
block (its `i_blocks` accounts for nothing beyond an external attribute
block), the **slow** form stores it as ordinary file data, and an
**inline-data** link keeps it in `i_block` and the `system.data`
attribute. An inode flagged inline on a volume without `inline_data` is
damage. Links are removed but never authored: `create_link` and `link`
answer `Unsupported`, which the VFS surfaces as `NotSupported`. Nothing
here reads a link's `i_block`, or a device's, as a block map.

## Directories

A child's kind comes from the directory entry's `file_type` byte when the
`filetype` feature is set, otherwise from the child inode's mode; unused
slots and the `.`/`..` self-links are skipped (the VFS resolves those
itself), and so are devices, pipes and sockets, which have no node kind
here. A directory holding its entries inline or encrypted is refused. A
directory is read one of two ways.

**Linear.** Every block is a leaf, scanned in order by record length. A
listing cursor is a record's byte offset: parsing resumes at the cursor's
block's first record and takes the first record at or past it, so a cursor
that is not a record boundary can only skip, never misparse. A record here
never moves while in use — removal folds it into its predecessor, insertion
splits a record's slack or appends a block — so an entry present throughout
a listing is listed exactly once.

**Hash-indexed** (`htree`, on a volume with `dir_index`). Block 0 is a root
mapping ranges of a name hash to leaf blocks, directly or through one level
of interior nodes. A lookup hashes the name and reads its leaf, and the
leaves after it while a run of that hash continues. A listing runs in
`(hash, name)` order and resumes just past the name it last returned —
leaves are packed and split here, so records do move, and no move disturbs
that position. Its cursor is a keyed digest of the name, never `0`.

- **Hashes.** The legacy, half-MD4 and TEA hashes, over bytes read as signed
  or unsigned as the superblock records, seeded by `s_hash_seed`. They are
  implemented from the format and the published algorithms they build on
  (MD4, RFC 1320; TEA, Wheeler and Needham, 1994) and checked against
  e2fsprogs' `debugfs dx_hash` as a black box.
- **New names** go into the leaf their hash selects. A full leaf whose free
  space is only scattered is packed first; otherwise it is split at a cut
  that keeps a hash's entries together when it can and balances the halves'
  bytes. A full root moves its entries down to a new interior level, and a
  full interior node splits; with both levels full a name is refused with
  `NoSpace`, as is one whose hash already spans 16 leaves.
- **A linear directory outgrowing its one block** on a `dir_index` volume
  is given an index: its names move to a new leaf, block 0
  becomes the root, hashed by the volume's default hash. A linear directory
  already longer than a block stays linear.
- **One transaction.** An insert's blocks — the new leaf, the inode mapping
  it, the index naming it — commit together (see [Writing](#writing)), and
  readers pass over an entry outside its leaf's hash range.
- **Checksums.** Under `metadata_csum` every index block's tail and every
  leaf's tail is checked before it is used and resealed when written; a
  mismatch fails closed with `DeviceFault`. A linear directory's leaves are
  checked the same way.
- **An index this driver cannot use** — a root of the wrong shape, SipHash
  (casefolded encrypted directories), a superblock recording no byte
  signedness, or a third level (`largedir`) — is read linearly, its names
  still found, listed and removed, and adding one is refused before anything
  changes (`DeviceFault` for damage, `Unsupported` otherwise).
- Without `dir_index` the index flag means nothing: the directory is linear,
  and adding a name clears the stale flag.

## Security

The driver makes no permission decision; the VFS judges every node against
the record `security(node)` reports. That record is the inode's mode (its
low twelve bits), owner and group (each joined from its low half and the
osd2 high half), with the `system.posix_acl_access` attribute folded in.
ext4 has no capability gate, so the record carries none.

### POSIX ACLs

The attribute is read where ext4 keeps it — the inode body after
`i_extra_isize`, or the external block `i_file_acl` names — in ext4's own
encoding: version 1, four-byte entries for the owner, owning group, mask
and other, eight-byte ones for named users and groups. (The version-2,
all-eight-byte form is what `getxattr` hands userspace, never what a
volume holds; it is refused as damage.)

An ACL with a mask becomes a POSIX.1e record (`AclModel::Posix`): the
owner is judged by its own triad, a named user by its entry under the
mask, a member of the owning group or of a named group by the union of
those entries under the mask — refused if they do not grant it, never
falling through to other — and anyone else by the other triad. The mode
reported mirrors the owner, mask and other entries, as it does on the
system that wrote it. A minimal ACL is the mode alone. A malformed ACL —
a wrong version, a torn or unknown entry, a missing or repeated required
entry, named entries with no mask, a subject named twice, more named
entries than the record holds — fails closed: the node cannot be judged,
so it cannot be reached.

`create` writes the owner, group and permission bits of the record it is
given in the inode it lays down, so a new node is never seen with another
owner; the format keeps no capability gate and a new node takes no ACL, so the
rest of the record is not stored. `set_security` stores a mode and an owner: a mode change moves the ACL's
owner, mask and other entries with it, copying an attribute block another
inode shares first. A record asking for a capability gate or a changed ACL
is refused whole rather than stored in part. A new file does not inherit
its directory's default ACL.

## Writing

Like the read surface, the write surface makes no permission decision: the
VFS authorises every write first.

**Transactions.** Every operation runs against the open transaction, its
metadata changes staged in `lib/writeback`'s dirty set and undone alone if
the operation fails. The transaction is published when the host's
write-back window closes, when it reaches its share of the machine's
write-back memory or a share of the journal, or on a sync; a handle with no
host, or a host with no clock, publishes every operation before reporting
it done. A long write or cut stops at the bound and reports the bytes it
stored, as `write(2)` does.

**Journaled volumes.** A transaction is logged after the one before,
circularly: descriptors and block images, a barrier — which also makes
durable the file data written in place for it and the previous
transaction's home writes — then the commit block and the journal
superblock naming it, then a second barrier, and only then the blocks home.
`needs_recovery` is set before anything is logged and stays set while the
volume is busy; settling a quiet volume makes the home writes durable,
empties the log durably, and only then clears the mark, so an idle volume
is clean and the mark never reads clear over a log holding a transaction.
A replay is only ever of the newest transaction, whose blocks no later
change can have reused: a block freed is reused only after the transaction
freeing it commits. Recovery reads whatever any writer left — revoke records,
64-bit tags, v2 and v3 checksums, escaped blocks, asynchronous commits —
and the writer adopts 64-bit tags and v3 checksums where the volume needs
them. A journal whose transactions would hold too
few blocks, or one a kernel aborted, leaves the volume read-only.

**Unjournaled volumes.** The superblock is marked not clean behind a
barrier, the blocks written home behind another, and the mark lifted; a
crash part-way leaves the volume for a checker.

**Failure.** A commit that fails before anything reaches the medium as the
new state is taken back whole; operations already reported done lose their
work with it, so then the handle stops writing. One that fails once the
medium carries the new state stops the handle too, the staged state kept
as its read view, for the journal's replay, a checker, or a later explicit
flush to finish.

**Allocation.** Blocks come near the inode's group and the previous block
of the file, in runs of up to one maximal extent; a lazily initialised
group's bitmap is derived from the layout and checked against its
descriptor's free count before it is first used, and a group whose
derivation disagrees is left alone. Directories spread across groups.
Freed blocks return to the bitmaps when their transaction commits.
Descriptors and inode records are verified on every read, even from a
block the transaction has staged.

**Files.** New files are extent-mapped on an `extent` volume and
classic-mapped otherwise; an extent tree splits and grows to the format's
depth. A write fills a hole with fresh zeroed blocks, converts an unwritten
range as it writes it, and overwrites in place. A truncate or delete frees
in bounded passes; one spanning transactions is put on the orphan chain
first and finished at the next mount if a crash interrupts it, the orphan
file a newer kernel keeps drained the same way. An orphan leaves the chain
from wherever it sits: the mount keeps each listed orphan's predecessor in an
index, rewound with the chain when an operation is undone or a transaction
lost, so the removal never walks the chain.

**Protected inodes.** The flags `chattr` sets hold against every principal,
the owner included. An immutable inode refuses every change: a write, a
truncate, a change of owner or mode, a rename, a removal, and a new entry in
an immutable directory. An append-only one takes writes at its end, and an
append-only directory new entries; nothing else.

### Timestamps

Every stamp a write takes is the host's wall clock,
`VolumeHost::wall_time` ([driver traits](../abi/driver_traits.md)),
following the POSIX model: a create gives the new inode all four stamps
and the parent directory a new `i_mtime`/`i_ctime`; a write or truncate
moves the file's `i_mtime`/`i_ctime`; a remove moves the directory's and,
for a node that keeps another name, its `i_ctime`; a rename moves the
node's `i_ctime` and both directories' `i_mtime`/`i_ctime`. A directory's
link-count change and its stamps share one inode write. A read moves no
stamp: the driver keeps no access time, so `i_atime` is the birth instant.

An enlarged record (`i_extra_isize` covering the `_extra` fields) holds a
stamp to the nanosecond from 1901-12-13 to 2446-05-10, each of the two epoch
bits adding 2³² seconds to the signed classic field as the ext4 disk layout
defines it, and holds a birth time. A classic 128-byte record holds whole
seconds within the signed 32-bit range and no birth time. A time outside
what the record holds, or no time at all while the clock is unset, is
written as the all-zero fields, which read back as `Time64::UNIX_EPOCH`, the
"no stamp" value: never a wrapped or clamped one. A freed inode's `i_dtime`
records when it went where the unsigned field holds that and cannot be read
as an orphan-list inode number, and a fixed non-zero marker otherwise.

### Checksums and wide descriptors

The write path maintains every on-disk checksum a volume carries. The
checksum primitives are first-party: a storage checksum is not a
cryptographic primitive.

- **`metadata_csum`** uses crc32c (reversed polynomial `0x82F6_3B78`,
  seeded with `s_checksum_seed` where the volume records one, else
  `crc32c(~0, s_uuid)`) — `lib/crc32c`'s, its dispatched
  hardware instruction continuing each seed as a raw register — for the
  superblock `s_checksum`, each group descriptor `bg_checksum`, the
  block/inode-bitmap checksums, every inode (`i_checksum_lo`/`hi`, seeded
  per inode by number and generation), each directory leaf's
  `ext4_dir_entry_tail`, each index block's `dx_tail`, each extent
  block's `ext4_extent_tail`, each attribute block, and each orphan-file
  block.
- **`gdt_csum`/`uninit_bg`** uses crc16 (reversed polynomial `0xA001`)
  for the legacy group-descriptor checksum.
- **`64bit`** descriptors carry the high halves of the bitmap checksums
  and `bg_itable_unused`, which the allocator also maintains.

## Formatting (mkfs)

`Ext4::format(block, inode_count, uuid)` lays a fresh, empty volume onto a
blank device, makes it durable, and opens it. It writes an unjournaled
`filetype` + `extent` + `extra_isize` volume with 256-byte inodes, 32-byte
descriptors, 4 KiB blocks from 128 MiB up and 1 KiB below, and every group
materialised. Only group 0 carries a superblock and descriptor table,
which `sparse_super2` with no backup groups declares. `inode_count` is a
minimum, rounded up to a whole number per group, at least 16; a device too
small for one group, a zero count or the nil `uuid` is refused with
`OutOfRange`.

## Capabilities

Loading requires `CAP_DRV_LOAD`; every other call is reached through the
handle the host minted at load. On removable media the driver is served from
user space ([Volumes served from user space](./served.md)): its `Run` binary binds `tairix,volume-ext4`, runs with
`CAP_FS_SERVE` and `CAP_LOG_EMIT`, opens the volume read-only when the kernel
says so and otherwise writable with its write-back bounded, and names the
generation the mount table shows.

## Tests

`cargo test -p tairix-drv-fs-ext4` runs each module's tests and the
driver's over hand-built images and volumes e2fsprogs 1.47.0 made
(`tests/fixtures`; the larger ones kept as `TXSPARSE` images, their
all-zero blocks left out):

- extent trees and the classic map: lookup, insertion with merging,
  splitting and growth to the depth limit, bounded truncation, marking
  unwritten ranges written;
- the journal: replay of `debugfs`-written logs with revokes and an
  escaped block, wraparound, revocation across a wrapped transaction id,
  a commit block whose write fails, and emptying a quiet log;
- allocation over lazily initialised groups, the reserve, deferred frees,
  descriptors verified in a staged block;
- ACLs from external blocks and inode bodies, a `chmod` moving the mask,
  a shared block copied before it changes, malformed ACLs refused;
- commits that fail before and after the medium changes, and a journaled
  commit torn after its commit block replayed at the next mount;
- holds: a held file keeping its blocks past its last link until its last
  unpin, holds counting, renames and a rename over a held file, a removed
  held directory taking no entries, `release_pins`, an orphan leaving the
  chain from its middle, tail and head, a lost commit leaving the chain and
  its index whole, a truncate spanning transactions listing a held orphan
  once, a held file a crash leaves unlinked released at mount, and an inode
  neither linked nor held answering `NotFound` to every call;
- and the read and write surfaces over the rest: listings, lookups,
  symbolic links, timestamps, hard links, renames, a kernel-sized stack.

`tests/checksummed.rs` recomputes every checksum of real `mke2fs` volumes
with an independent crc after a write cycle.

`tests/fuzz_ext4_mount.rs` is the fuzz harness `cargo xtask fuzz` runs: it
mutates a formatted volume and the e2fsprogs fixtures — a journal needing
recovery among them — and opens each read-only, then writable, replaying the
journal and finishing orphans, walking, writing — a file held across its
unlink among the writes — committing and remounting; nothing may panic or
hang.

## Extended-metadata representability

The ext4 driver does **not** implement the `FilesystemAttrs` ABI (its
attribute facet answers `None`, so the `fs_attr_*` syscalls refuse an ext4
mount with `NotSupported`), so it cannot
represent ARXFS extended attributes or foreign-filesystem preset metadata
(`arxfs-spec.md` §21). Under the cross-filesystem preservation contract an
exact-preservation copy of a file carrying such attributes onto ext4 reports
`MetadataNotRepresentable`; a best-effort copy drops them only under an
explicit, documented lossy policy. (ext4 attributes are read only for POSIX
ACLs and inline data, not as a general key/value store.)
