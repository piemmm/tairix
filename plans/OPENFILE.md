# OPENFILE — node-bound open files, create modes and sticky directories

| Id | Item | Status |
|---|---|---|
| OF1 | Sticky directories: in a sticky directory only an entry's owner or the directory's owner removes, renames away or replaces it (D1366) | done |
| OF2 | `fs_sync` flushes only the descriptor's own volume (D1369) | done |
| OF3 | Byte writes keyed on the node, `parent`, `NodeInfo::ino`, FAT location ids | done |
| OF4 | Driver pins and deferred deletion on every format | done |
| OF5 | Open file descriptions bound to a pinned node of one projection; the descriptor limit enforced | done |
| OF6 | File locks, directory watches and wait-set members keyed on the pinned node | done |
| OF7 | Create modes, the per-process umask, setgid directories (D1381, D1368) | done |
| OF8 | Descriptor-relative calls, a node-bound working directory, one resolver, and the access query a program asks whether it may read, write or run a node through (D1420) | done |
| OF9 | The command tools walk by descriptor — `cp`, `mv`, `rm`, `chmod`, `chown`, `getcap`, `setcap` — as `replace_file` stages, so no entry can be swapped for a link between reading and changing it, and every tree copy gives what it makes its source's mode (D1365, D1367, D1378, D1422, D1425, D1485) | done |
| OF10 | `fstree` and the file manager walk by descriptor too, through the shared resumable copy, move and removal engine, which holds the walk as a cursor between a paused or time-sliced operation's units | done |

## The model

An open file description is bound, for its whole life, to one node of one
projection. Nothing after open looks at a path.

- **`PinnedNode` / `NodeRef`** (`kernel/core/src/fs/node.rs`). One driver pin
  per open; clones of a `NodeRef` share it. Until the last drops, the pinned
  id names the object through renames, writes and the loss of its last name.
  The drop queues the id for release (one spin lock, no allocation, no park).
- **`VolumeNodes`**: the kernel's record of one *registration* of a volume
  driver — a never-reused instance number, a `retired` flag set by detach,
  retraction, recovery and a clean surprise removal, and the release queue.
  Taking a pin reserves its release slot, so a release never allocates.
- **`Projection`**: a view binding, shared by every description opened
  through it, its flags, class, template and subtree fixed for life; `live` is
  cleared when it leaves the table, and backing one replaces it.
- **`NodeHandle`**: what a node-backed descriptor holds — the `NodeRef`, the
  projection, the `Authority`, `opened_as` (the canonical spelling at open, a
  label never resolved) and `at_floor` (the node is its projection's floor,
  whose listing shows the projections attached beneath it). `Authority::Holder`
  is the opener's own descriptor, each operation running as whoever makes it,
  so a capability narrowed while it is open stops working at once;
  `Authority::Frozen` carries the identity captured when the descriptor was
  conferred at spawn or granted, a grant with its write ceiling.
- `OpenBacking` is `Node | Catalog | Resource | Pipe | PtyMaster | PtySlave`.
  The working directory is a `Cwd` beside the process's umask in its
  `ProcessFs`: `Root` until the root volume is mounted, then a searchable
  directory's `NodeHandle` or the catalog's `Projection`, held by node as a
  descriptor is.

Invariants:

1. While a node is pinned, every id the driver reports for it — lookup,
   listings, `parent`, the root — is its pinned id, so every kernel key agrees.
2. User-visible identity is `NodeInfo::ino`: `FileId` is `(volume, ino)`.
   Pinned ids are internal handles and never reach userland.
3. Descriptors sharing one description reach one node through one pin; a
   descriptor's authority is the opener's live identity for the opener's own
   descriptor and frozen for one conferred or granted.

## Lifetime

Open walks once, creates what it was asked to, reserves the release slot and
pins, under one hold of the driver lock. `fs_close` reaps with a `try_lock`,
so a close never waits behind a long truncate; every other drop path (exit,
spawn unwind, redemption, unmap) marks the volume due and wakes the
write-back flusher, whose pass reaps before it publishes. Drops run outside
the registry lock. `unpin` answers `Held`, `Released` or `Freed`; the last pin
of a nameless node frees it there, and the cache drops what it kept under the
id.

No pin survives a crash. A writable mount finishes its format's orphan record
before it serves: ARXFS's pending-delete set, ext4's orphan chain. FAT, UDF
and ADFS keep no such record, so a crash with a deleted-but-open file leaks its
space to the format's own checker — renaming into a hidden directory would
pollute another system's view, and the alternatives are non-standard.

## What is checked, and when

| Moment | Identity | Checked |
|---|---|---|
| Resolution (open, every descriptor-relative lookup, chdir) | the caller | `CAP_FS_ACCESS` at dispatch; search on every directory looked up in, the start included; mount flags; on open, the access the descriptor will carry; for a name mutation, write and search on the directory, the occupant's gate, the sticky rule, a writable mount |
| Byte operations (read, write, truncate, punch, seek_region, stat, readdir, sync, lock, `file_map` faults) | the description's authority | the descriptor's access; projection live, volume not retired (else `Stale`); `CAP_FS_ACCESS` on the authority's set; the node's gate, and its mode and ACL unless the authority owns the node through a descriptor that is not a grant, re-checked every call except by stat and sync, which like POSIX's `fstat` and `fsync` check neither; a grant's extent ceiling |
| Metadata operations (mode, owner, attributes) | the caller | today's owner-only and `CAP_FS_CHOWN` rules, the gate, a writable mount |
| Never | | search permission above a held node — a descriptor is not a name |

The per-call re-check keeps today's revocation by mode, ACL, group and gate,
so no principal gains anything. The owner exemption is what POSIX requires of
`open(O_CREAT, 0444)` then `write`, and removes nothing: an owner may restore
any bit. A grant never has it, so an owner's chmod still revokes a grant.
Metadata calls run as the caller, so a child handed a system-owned descriptor
cannot chmod it with the opener's rights. Removing search permission on a
parent no longer cuts off descriptors below it; that is POSIX behaviour.

The filesystem service keeps each principal's groups as the identity table of
one generation resolved them, the table's install and every replacement moving
the generation, so the common call allocates nothing; the cost is one driver
lock and one cached security lookup, against a whole-path walk.

## Resolution

One resolver. Steps are names and `..`, each tagged with who supplied it;
`.` is dropped and `..` is never collapsed as text. A walk starts at the view
root (absolute), the working directory, or a directory descriptor's node; an
absolute path ignores the descriptor. Within a projection the walk climbs with
`parent()`. At a projection's floor a typed `..` continues at the parent
projection's root and a child projection's name continues at its floor; a
`..` or absolute target a link supplies clamps to the link's own projection.

Every projection's parent path is the root of another projection or the
catalog, and an attach that breaks this is refused, so a crossing happens only
at a floor. A crossing releases one driver lock and takes the next from a
stable start (a floor or a pinned node). A single-path operation walks its last
segment and mutates under one hold; a two-path one pins both parents, refuses
`CrossVolume` across projections, then mutates under one hold. In a removed
directory, lookup and `parent` answer `NotFound`, creation is refused and a
listing is empty. `canonical_of` re-resolves `opened_as` and checks the node;
failing that it climbs with `parent()`, finding each name in its parent's
listing under read permission.

## Locking

A driver's sleeping lock is outermost. Under it may be taken the mount
table's lock, the driver registry's lock, a volume's release queue and a
watch table, never the other way round: what the table or the registry holds
is copied out and the guard dropped before a driver is locked. A projection's
liveness and a registration's retirement are read after the driver's lock is
taken, under which a detach changes them; a surprise removal, whose context
may not sleep, retires without it, and an operation already past the check
fails against the departed device. The release queue's lock is a leaf, with
only the kernel heap's own lock beneath it, taken when an open reserves a
pin's release slot. A drop takes the queue and, when it makes the
registration due, sets the write-back queue's lock-free wake flag, so a burst
of drops wakes the flusher once; a reap takes one release at a time, so it
allocates nothing and never calls the driver under the queue's lock. The
service's groups cache is a leaf as well, consulted before any driver is
locked; a miss resolves from the identity table outside it. Nothing parks
under a spin lock, and no interrupt handler touches any of these. The release
queue's interleavings are enumerated by a deterministic host test, since loom
cannot build `kernel/core` (D131); a close against an open, an unlink against
a read, a detach against a descriptor and a rename against an open are
host-tested in each order.

## Detach, removal, revocation

A plain detach reaps, then answers `Busy` while pins remain. A force detach,
under the driver lock, reaps, has the driver release every pin (completing each
deferred deletion), retires the registration, commits, swaps in the stand-in
and marks the projections dead: revoked descriptors answer `Stale`, and the
audit records the count. A surprise removal with work unpublished keeps the
registration through its stand-in, so descriptors answer the device error
while the volume is away, and a clean one retires it; retraction, recovery and
a verified re-insert each start a fresh registration,
so only new opens see the returned volume. `Errno::Stale` means "the handle no
longer reaches what it named". An open that waited out a detach finds nothing
at its path.

## Delegation, spawn, mapping, locks, watches, sync

- `fd_grant` makes a new description over the same `NodeRef`, frozen to the
  grantor's identity with a grant ceiling; a relay keeps the first grantor's
  authority and the smaller ceiling. `fd_grant_location` tells the
  `opened_as` label, which a rename can make stale. Spawn wiring shares the
  description and gives the child's descriptor the same pin, frozen to the
  parent's identity at the spawn. Directories are refused both ways.
- A `file_map` region holds a `NodeHandle`, so the node outlives an unlink and
  a close; each fault applies the byte-operation checks.
- File locks key on `(registration, pinned)`: a rename moves nothing and a FAT
  write keeps the key. Descriptions shared by spawn are lock subjects; a grant
  is not.
- Watches and wait-set members key on the pinned node. A drain checks only
  the watched directory — live, not removed, readable — so renaming it or any
  directory above it ends nothing, and its own security changing makes its
  watchers rescan. The cache keeps each pinned node's place for its whole
  hold, moved by a rename and dropped by a removal, so a change through a
  descriptor reports the name the node bears now.
- `fs_sync(fd)` flushes the descriptor's own volume; a catalog descriptor has
  nothing to flush.

## Create modes, umask, setgid, sticky

A node is born with its whole security record: the requested mode less the
creator's umask (a link is `0777`), the directory's group when the directory
is setgid (which a new directory inherits), the creator's group otherwise, and
setgid dropped from a file whose group the creator is not in. The umask is per
process, `022` from PID 1, inherited at spawn, set by `fs_umask`.

The dispatcher refuses a mode past `FS_MODE_MASK`, and any mode on an open
without `CREATE`. The mode and the caller's umask travel as one `CreateMode`
through `FilesystemService::{open, mkdir}` into the one creation record
(`delegate.rs`'s `creation_record`), the node's own record from birth. The umask lives in the process's `ProcessFs`
in the address-space registry, `fs_umask` (no. 153) answering the mask it
replaces and `FS_UMASK_QUERY` reading it unchanged.

In a sticky directory only the entry's owner or the directory's owner removes,
renames away or replaces it, the occupant's gate still applying; no capability
overrides it — a `CAP_FS_CHOWN` holder takes ownership explicitly, and that is
audited.

## Bounds

Pin tables and held sets are bounded by distinct pinned nodes, the release
queue and the cache's place map by live pins, ext4's in-memory orphan index by
the pins held this mount, a walker by one descriptor a level. The descriptor
limit is `OpenFiles`, enforced on every descriptor a process gains, its default
one per 64 KiB of RAM and never below 1024.

## The driver contract

`parent` is on the read trait and `NodeInfo` carries `ino`, equal on every
format to the raw value of the id lookups answer; byte writes take the node,
and a FAT file's id is its short entry's location (OF3). `pin`, `unpin` and
`release_pins` are on the read trait with no defaults, so every format, the
in-memory test driver and every wrapper states its own; the wrapper
conformance suite checks that a hold and its deferred deletion reach the
driver. `create` and `create_link` take the record the node is born with:
the asked mode less the umask, owned by its creator, in a setgid directory's
group. `link`, `remove` and `rename` keep `(dir, name)`
and keep a pinned node that loses its last name until its last `unpin` frees
it (`Unpinned::Freed`). A natural id is valid for a byte mutation only until
the next name mutation in its directory, and on a content-derived format until
the next mutation of the node; a pinned id until its last unpin. A nameless
pinned node still takes byte I/O, security and attribute changes; a removed
directory takes no entries; a node no name and no hold reaches is `NotFound`
to every call. OF5's descriptions take the kernel's first pins, and from then
the kernel writes only through pinned ids.

| Format | Pins and deferred deletion |
|---|---|
| ARXFS | held inodes; the pending-delete set skips them; the last unpin of an unlinked inode reclaims it |
| ext4 | held inodes; the last unlink of a held inode joins the orphan chain in the same journalled operation; the last unpin releases it |
| FAT32 | file ids are the short entry's location; pinned entries get tokens; a removed pinned file keeps its chain until the last unpin |
| ADFS | tokens keyed on the parent and folded name; fragments freed at the last unpin |
| UDF | held ICBs; an entry at link count 0 kept until the last unpin |
| ISO 9660 | stateless pins |

## Calls

Descriptor-relative throughout: open, mkdir, unlink, symlink, readlink,
realpath, chdir, set_mode, set_owner and access resolve a relative path from a
directory descriptor (`FD_CWD` for the working directory), and rename and link
take two `PathAt` records; an absolute path ignores the descriptor. No handler
spells a path against the working directory. Chdir, set_mode, set_owner and
access take `AtFlags`: `NO_FOLLOW` names a final link itself (chdir refuses
it) and `EMPTY_PATH` the node the descriptor holds. Open and mkdir take a mode.
Attribute calls are by descriptor only. Metadata through a descriptor runs as
the caller. `fs_access` (no. 154) answers as the caller, or with `EMPTY_PATH`
under the descriptor's own authority: the node's verdict for each `Access`
bit, then the projection's flags. Unlink and rename take an optional
`NodeExpect` (`EXPECT_NODE`, `EXPECT_UNCHANGED`), copied only when a flag asks
for one and checked under the volume lock, a mismatch answering `Stale`;
`CREATE | DIRECTORY | EXCLUSIVE` makes a directory and returns it open,
closing the race between mkdir and open. `fs_getcwd` answers the working directory's `canonical_of`. `fs_umask`
sets and queries the mask. No new error codes.

A containment flag (`RESOLVE_BENEATH`-style) lands with its first consumer.

## Walking a tree

`lib/fscopy::tree` is the seam a first-party tool walks a tree through. A
`Dir` is a directory held open: each child is reached by name from it with
`NO_FOLLOW` (`open_dir`, `open_file`, `open_node`), and `walk` descends depth
first through held directories only, each listing read whole first. A change
goes through the descriptor of the node it was read from, and a removal or
rename names the node it expects (`EXPECT_NODE`, `EXPECT_UNCHANGED`), so a name
swapped meanwhile answers `Stale` and is left alone. `tairix_rt::copy`
implements the seam (`File`, `Dir`, `WorkingDirectory`); `fixture::Tree` is its
in-memory model, with an attacker hook that runs between any two calls.

- `mv` across volumes builds its copy owner-only under a staging name, fills
  it through descriptors recording each copied entry's `NodeExpect`, syncs,
  renames it into place with `EXPECT_NODE`, and removes only what it copied
  while unchanged; an entry added or changed meanwhile is kept and reported.
  `replace_file` stages owner-only and renames the same way.
- `rm -r` removes a directory with `DIRECTORY | EXPECT_NODE` after its
  entries; `chmod`, `chown`, `getcap` and `setcap` change each node through its
  own descriptor. None descends or changes through a link met inside a tree.
- A copy makes a file in its source's permission bits and a directory in its
  permission and sticky bits, each less the umask (`file_mode`, `DirModes`): a
  directory is filled holding its owner's read, write and search and given its
  own mode once full, through the handle held on the directory made. `cp`,
  `fstree` and the file manager's paste all do.

`Cursor` is the walk with its place kept: it holds the chain of directories it
is inside and the listing of each, so it costs the tree's depth and the
listings along the path, and `walk` is the loop over it. An operation that
pauses between units holds one.

`lib/fscopy::op` is the one resumable copy, move and removal built on it. An
`Operation` advances a bounded number of units — an entry acted on, or a chunk
of a file — and pauses on a taken name (`Taken::Ask`) or fails on it
(`Taken::Refuse`, for a caller that chose free names). Its source carries the
node a listing showed under the name (`Named::seen`), and one that has come to
hold another is refused before anything changes. An overwrite builds the
replacement beside the occupant and renames it over the occupant once whole
and on the medium, only while that is the node asked about. A copy gives what
it makes `cp`'s modes; a move is one rename on its own volume and, across
volumes, a copy that keeps its source's mode and owner (`carry_owner`, as
`mv`), is put on the medium, and then removes only what it copied while
unchanged; a move replaces a directory only while it is empty. A removal takes
entries before their directory and keeps an entry put in its place meanwhile.
A copy that fails or is cancelled is wound down first: the directories it made
are given their modes and a file part-written is removed. `fstree`'s `FileOp`
drives one to its end or its question on the key loop; the file manager runs
one per item of a paste, a delete or a move to Trash on its operations worker,
a unit per step.
