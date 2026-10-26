# `tairix-drv-fs-ext4` — ext2/ext3/ext4 filesystem driver

Serves an ext2, ext3 or ext4 volume behind any
`tairix_abi::driver::block::Block` device through the versioned
`FilesystemRead`, `FilesystemWrite`, `FilesystemSecurity` and
`FilesystemStats` traits. It makes no permission decision; the VFS applies
the owner, mode and ACL it reports. The design is
`docs/src/filesystem/ext4.md`.

Stability tier: **experimental**.

## Volumes it serves

| On-disk feature                                        | Read | Write |
|--------------------------------------------------------|------|-------|
| ext2, ext3 and ext4 layouts, block sizes 1–4 KiB        | yes  | yes   |
| 128-byte and larger inodes, 32- and 64-byte descriptors | yes  | yes   |
| Extent trees of any depth (`extent`)                    | yes  | yes   |
| Classic block map: direct, single, double, triple       | yes  | yes   |
| Unwritten (preallocated) extents                        | yes  | yes   |
| Linear and hash-indexed (`dir_index`) directories       | yes  | yes   |
| `largedir`, `dir_nlink`, `huge_file`, `large_file`      | yes  | yes   |
| `flex_bg`, `meta_bg`, `sparse_super`, `sparse_super2`   | yes  | yes   |
| Lazily initialised groups (`BLOCK_UNINIT`, `INODE_UNINIT`) | yes | yes |
| `metadata_csum`, `metadata_csum_seed`, `gdt_csum`       | yes  | yes   |
| Internal journal (`has_journal`), replay of a crashed one | yes | yes |
| The orphan chain and the orphan file (`orphan_file`)    | yes  | yes   |
| POSIX access ACLs                                       | yes  | mode changes |
| Inline data (`inline_data`)                             | files and links; inline directories are refused | no |
| `bigalloc`, `mmp`, `encrypt`, `casefold`, `ea_inode`    | yes, but encrypted names are refused and casefolded ones matched exactly | no |

A volume carrying a feature the write path does not maintain, errors a
checker recorded, or a crash a checker has not yet seen (an unclean
unjournaled volume) opens all the same and serves reads.
`Ext4::write_refusal` names why it refuses writes, and the volume service
mounts such a volume read-only and audits that reason, so a refused write
reads as "read-only file system".

`Ext4::generation` names the generation a volume's features make it, which
the volume service reports as the mount's type. Writing never moves a volume
to a later one: new files stay block-mapped without `extent`, descriptor
fields only ext4 defines stay zero, a journal keeps its tag format, and a
revision-0 volume is raised to the dynamic revision before a field it lacks
is stored.

## Crash consistency

Metadata changes batch into transactions, published when the host's
write-back window closes, when a transaction reaches its memory bound or
its share of the journal, or on a sync:

- **Journaled (ext3, ext4):** the transaction is logged after the one
  before, circularly, then committed behind two barriers, and only then
  written home. `needs_recovery` is set before anything is logged; a quiet
  volume's log is emptied durably, and only then is the mark cleared, so
  an idle volume is clean. Opening a volume writable replays a log a crash
  left, any writer's (revokes, 64-bit tags, v2/v3 checksums, escaped
  blocks), then reads the volume again from what the replay left; a volume
  under multi-mount protection is never replayed.
- **Unjournaled (ext2):** the volume is marked not clean behind a barrier,
  the blocks written home behind another, and the mark lifted.

File data is written in place before the commit that maps it. Blocks a
transaction frees are reused only after it commits, so a replay never
writes old metadata over new file data. A truncate or delete too large for
one transaction is listed on the orphan chain first and finished at the
next mount if a crash interrupts it.

An inode the kernel holds (`pin`) that loses its last link is listed on the
orphan chain in that same operation and kept, readable and writable, until its
last `unpin` releases it; after a crash the chain alone remains and the mount
releases it. A held orphan leaves the chain from wherever it sits through an
in-memory index of each listed orphan's predecessor, never a walk of the
chain.

## Security

`security` reports an inode's mode, owner and group, and folds its
`system.posix_acl_access` attribute into a POSIX.1e record: named entries
under the mask, the owning group's own entry, the mode's group triad
mirroring the mask. A malformed ACL fails closed. `create` gives a new node
the owner, group and permission bits of the record it is handed; the format
keeps no capability gate and a new node takes no ACL. `set_security` stores a
mode and an owner (a `chmod` moves the ACL's mask with it, copying a shared
attribute block first) and refuses a capability gate or an ACL change it
cannot store. New files do not inherit a directory's default ACL. The
immutable and append-only flags `chattr` sets hold against every principal,
the owner included.

## Limitations

- Symbolic and hard links are read and removed, never created.
- Inline-data directories and encrypted names are refused, and a lookup
  in a casefolded directory matches names exactly.
- Block sizes above 4 KiB and external journals are refused at open.
- `Ext4::format` lays an unjournaled `extent` volume with 256-byte inodes,
  only group 0 carrying a superblock (`sparse_super2` with no backups).

## Hosting

A removable ext volume is served from user space
(`docs/src/filesystem/served.md`): the crate's `Run` binary, built with the
`program` feature, binds `tairix,volume-ext4` (`BIND_KEYS`) and serves the
volume through `lib/fsserve` (`Served`), its write-back bounded to the
machine's memory and pressure. The kernel links no ext driver.

## Required capabilities

`CAP_DRV_LOAD` at `register`; every other call is reached through the
handle the host minted at load. The served driver runs with `CAP_FS_SERVE`
and `CAP_LOG_EMIT` and reaches only the volume its node's grant names.

## Tests

`cargo test -p tairix-drv-fs-ext4` covers each module against hand-built
and e2fsprogs-made images (`tests/fixtures`, the large ones stored sparse):
extent and block-map edits, the journal against `debugfs`-written logs,
allocation over lazily initialised groups, ACL decoding, commit failures
before and after the medium changes, replay after a crash, and holds: a held
file kept past its last link until its last unpin, a removed held directory
taking no entries, the orphan chain's predecessor index across a lost commit,
and a held file a crash leaves unlinked released at mount.
`tests/generations.rs` writes real ext2, ext3 and revision-0 images and checks
each is left the generation it was made.

`tests/fuzz_ext4_mount.rs` is the fuzz harness `cargo xtask fuzz` runs: byte
flips, random rewrites and shaped values — coordinated superblock fields and
feature bits with the checksum re-sealed, extent-header counts and depths, and
the geometry of a logged superblock a replay writes home — over a formatted
volume holding a fragmented file and the e2fsprogs fixtures, opened read-only,
then writable (replaying the journal and finishing orphans), walked, written,
committed and opened again, none of which may panic or hang.
