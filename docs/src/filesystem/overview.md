# Filesystem overview

TAIRiX separates **filesystem policy** from **filesystem I/O**:

- **Policy** — path resolution, the mount table, the on-disk layout rules
  of `AGENTS.md` §16, and the §5.3 permission model — lives in
  `kernel/core::fs` (the VFS). It is architecture-neutral and depends only
  on `lib/abi`, `lib/caps`, and `kernel/sec`.
- **I/O** — reading and writing blocks and parsing an on-disk format —
  lives in the `drivers/filesystem/*` crates (`arxfs`, `ext4`, `fat32`)
  behind the [`Filesystem`] trait in `lib/abi`. The VFS never duplicates a
  driver's block I/O. The frozen `Filesystem` trait is mount/unmount only;
  path I/O delegates to a driver through the separate versioned
  `FilesystemRead` and `FilesystemWrite` traits (`AGENTS.md` §2.4 / §9).
  The first block-backed driver is the read/write [FAT32 driver](./fat32.md);
  the native [arxfs driver](./arxfs.md) adds a copy-on-write
  filesystem that stores per-inode ACLs and capability gates; the
  read/write [ext4 driver](./ext4.md) serves ext2, ext3 and ext4 volumes,
  checksums and journal included, and surfaces each inode's stored
  owner/mode through `FilesystemSecurity`.

This page describes the VFS. The on-disk layout it enforces is in
[Layout](./layout.md); the permission model is in
[Permissions](./permissions.md). The native [arxfs](./arxfs.md) format —
copy-on-write, checksummed, compressed and deduplicating, encrypted or
plaintext as chosen when a volume is formatted, grown in stages behind these
same traits — is the one
native format; there is no separate `v1`.

## The VFS tree

The VFS is a tree of inodes. Each inode is either a **directory** (a map
from name to child inode) or a **file** (a byte payload), and carries the
access-control [`Metadata`] described in [Permissions](./permissions.md).

Before a block-backed driver mounts, the tree is held in RAM — the natural
shape of the boot-time root filesystem. The structure and every policy
check are identical regardless of what eventually backs a subtree, so a
later block-backed mount changes *where the bytes live*, not *how access is
decided*.

## Operations

All operations take the caller's [`Credentials`] (uid, primary gid,
supplementary gids, and capability set) and an absolute [`Path`]:

| Operation             | Effect                                            |
| --------------------- | ------------------------------------------------- |
| `metadata`            | Stat an inode (search permission on the path).    |
| `mkdir`               | Create a directory.                               |
| `create_file`         | Create a regular file.                            |
| `read` / `write`      | Read or replace a file's contents.                |
| `list`                | List a directory's entry names.                   |
| `remove`              | Remove an empty directory or a file.              |
| `set_required_cap`    | Set the per-inode capability gate (owner only).   |

Every operation resolves the path component by component, enforcing
**search (execute) permission** on each directory it descends through, and
then applies the access check appropriate to the operation. All failures
are reported as a [`VfsError`]; `VfsError::to_errno` maps them to the
stable user/kernel `Errno` at the syscall boundary.

## Driver delegation

A subtree can be backed by a `drivers/filesystem/*` driver instead of the
in-RAM arena. Every path operation goes through one resolver
(`kernel/core/src/fs/vfs.rs`): it walks the projections no volume backs
itself, hands each backed one to its driver's walk
(`DelegatedFs::walk_segment`) under one hold of that driver's lock, and
crosses from one projection to the next only at a floor
([path resolution](#path-resolution)). The operation then acts on what the
walk reached under the same hold of the driver it ended on, judged by that
projection's flags, so nothing between the walk's last step and the change
can move the name it acts on. A rename or a link keeps the directory (for a
link, also the node) each of its paths reached pinned between the two walks,
refuses two projections with `VfsError::CrossVolume`, and changes the volume
under one hold, with no retry.

`MountedFilesystemService` serves every mounted volume through the resolver.
A boot reader holding the one driver of the volume it reads uses the same
resolver through the two `Vfs` methods below, which take an absolute
[`Path`] and a driver implementing `FilesystemRead` and `FilesystemSecurity`;
a walk that goes on from that volume onto another is `VfsError::CrossVolume`:

| Operation                  | Effect                                                    |
| -------------------------- | --------------------------------------------------------- |
| `list_via_secured`         | List a directory, resuming from a position.               |
| `read_bounded_via_secured` | Read the leading bytes of a regular file of bounded size. |

Every walk asks **search permission on every directory** it passes through,
whichever projection shows it. The driver returns *structural* I/O only (node
kind, size, children, bytes); it makes **no** permission decision. The check
runs against a node's [`Metadata`] before each read, before each mutation
(which also requires **write** permission on the parent and refuses a
`READ_ONLY` mount), and on every directory descended into. An unrecoverable
driver fault, or a directory entry whose on-disk name is not valid UTF-8,
surfaces as `VfsError::Io`.

A driver's *structural* refusals are carried across unchanged rather than
re-derived per call site: `DriverError::AlreadyExists` is
`VfsError::AlreadyExists` (`EEXIST`), `DriverError::DirectoryNotEmpty` is
`VfsError::NotEmpty` (`ENOTEMPTY`), and `DriverError::DirectoryCycle` — a
rename that would make a directory its own descendant — is
`VfsError::DirectoryCycle`, distinct because emptying the destination can
never make that move lawful. A full volume (`DriverError::NoSpace`) is
`VfsError::NoSpace` (`ENOSPC`) and exhausted memory is
`VfsError::OutOfMemory`, neither an I/O error. One mapping serves every
operation, so the answer a caller sees does not depend on which one met the
conflict.

Kind is the VFS's own question. Every walk refuses a file used as a directory
(`VfsError::NotADirectory`), every byte operation refuses a directory
(`VfsError::IsADirectory`), and a rename between kinds is refused before the
driver is asked — `NotADirectory` when a directory would replace a
non-directory, `IsADirectory` the other way round. So a driver's
`DriverError::Unsupported` only ever means what the format cannot do, and is
`VfsError::NotSupported` wherever it arrives: a link on a format that stores
none, a mode or owner change on one that stores no record of its own.

A create hands the driver the whole record the new node is born with, in the
creating call: the creator's identity, the mode the create asked for less its
process's umask (`0o777` for a link) and, in a setgid directory, that
directory's group. There is no second step, so a refused create leaves no
half-made name for a retry to collide with. The driver keeps what of the
record its format stores.

Every node is judged by **its own stored record**, read through the driver's
`FilesystemSecurity` surface and translated (`Metadata::from_node_security`):
[arxfs](./arxfs.md) stores full per-inode owner, mode, ACL, and capability
gate, so a file marked owner-only or gated on a capability is enforced as
stored; the [ext4 driver](./ext4.md) reports each inode's stored owner and
mode with its POSIX access ACL folded in, judged under the POSIX.1e model
(named entries under the mask, the owning group by its own entry); and FAT32,
which stores no owner, reports the record its volume's storage-group identity
map assigns, or its own restrictive system-owned posture where no map is
provisioned.

The in-RAM tree and every volume feed the *same* `Metadata::authorize`
decision, so the policy is single-sourced; only the metadata's origin changes.

A driver-backed mount normally sits *below* the root (`/Storage/usb0`,
…), but the **root mount itself** can also be given a backing driver
(`MountTable::back_root`, exactly once — a second root volume is refused)
— the shape of a real installation, whose root volume carries the whole
`AGENTS.md` §16 tree from its own root directory. The kernel's boot-time
users-database load (`tairix_kernel_core::users::load_users_db`, see the
[kernel page](../architecture/kernel.md)) reads
`/System/Security/Users` through exactly this path.

The whole driver-backed read **and** write path is exercised end-to-end
under QEMU against a real (emulated) virtio-blk device by three verticals:
the `fat32_virtio_blk_pci_x86_64` vertical mounts a planted FAT32 image
through the FAT32 driver (see [FAT32](./fat32.md)), the
`arxfs_virtio_blk_pci_x86_64` vertical mounts a planted arxfs volume —
one the arxfs driver itself authored — through the arxfs driver (see
[arxfs](./arxfs.md)), and the `users_db_qemu_aarch64` vertical mounts a
planted users-root arxfs volume on the aarch64 `virt` board and drives
the kernel's users-database load against it. The first two round-trip a
read and a write through the shared, transport-generic device tail.

## Path resolution

A caller's path is a `/`-view path, or a relative one resolved from where the
caller stands: the view root, its working directory, or a directory it holds a
descriptor of (`At`, `Start`). An absolute path ignores the start. The path is
parsed into steps through `lib/path` (`steps`): `.` is dropped and `..`
is kept, never collapsed as text, so each `..` leaves the directory the walk
actually reached — the node it came through, or the driver's `parent` of a
directory it started at. A `..` needs search permission on the directory it
leaves, and a file has none (`VfsError::NotADirectory`). An empty or over-long
component, a NUL byte, or more than `MAX_PATH_COMPONENTS` steps is
`VfsError::InvalidPath`; an alias or `id::` spelling is turned into the view
location it names before it reaches the VFS.

At a projection's floor a caller's `..` climbs into the projection it is shown
in (`/..` is `/`), and a caller's name that another projection is shown under,
matched as the floor's volume matches its own entries, goes on at that
projection's floor. Every projection but the root's sits
directly beneath another's floor, so the floor is the only place a walk
crosses. A step a symbolic link supplied never crosses ([below](#symbolic-links)).

A floor whose volume no driver serves yet — `/` before the root is unlocked —
is still crossed, by the exact name of a projection shown beneath it or by a
`..`, its search permission asked of the record that stands for it (a runtime
projection's template, or its node in the boot layout). Every other step there
needs the volume itself and is `VfsError::Unserved`.

A working directory (`Cwd`) and a directory descriptor are the directory
itself, not the path they were entered by: a relative path from one is walked
from the pinned node, whose search permission is asked as the caller on every
call, and one whose projection has left the mount table, or whose
registration is retired, is `VfsError::Stale`. A directory no name reaches any
more holds no names: nothing in it is found or made, nothing moves or links
into it, and its `..` is `VfsError::NotFound`.

A [`Path`], what the one-driver API takes, is absolute and normalised at parse
time, so it spells no `..`. A *symbolic link's stored target* is on-disk data
rather than a caller's spelling and has its own grammar (`parse_link_target`).

## Symbolic links

A node may be a `NodeKind::Symlink`, whose content is a **path** rather
than bytes. Resolution follows one per component, under two fixed
fail-closed bounds — at most `SYMLINK_HOP_MAX` (40) hops and
`MAX_RESOLVE_STEPS` steps in one resolution — so a cycle or an over-long
chain is refused with `VfsError::LinkLoop` instead of being walked. These
are security bounds on untrusted on-disk structure, not capacities
(`AGENTS.md` §24.4). The full design, including the decisions this page
only summarises, is `plans/SYMLINKS.md`.

- **A link in an interior position is always followed**: such a component
  is being *used* as a directory, so what matters is what it names. Only
  the **final** component's treatment varies, selected by `FinalLink`
  (`tairix_abi::fs`, the one definition the kernel and every tool share):
  `Follow` is POSIX `stat`, `Keep` is POSIX `lstat`. `Keep` is how an
  `OpenFlags::NO_FOLLOW` open resolves (`FinalLink::for_open`), and the
  descriptor holds the node that open chose — so a stat or a listing through
  it can never contradict the open that produced it. A dangling link is
  describable only under `Keep`; `Follow` reports it as `VfsError::NotFound`.
- **`..` is resolved physically, never lexically.** The walk keeps a stack
  of the real nodes it passed through and `..` pops that stack, so it names
  the directory the resolution actually came through rather than one a
  link's spelling suggests.
- **A link cannot resolve outside the projection that shows it.** Its `..`
  stops at that projection's floor, its absolute target restarts there, and a
  name it supplies that a projection beneath that floor is shown under is
  `VfsError::NotFound`, neither crossing into that projection nor reaching
  the directory it covers, so a USB stick's link cannot name
  `/System/Security`. For a
  **sub-mount** (a subtree of a larger volume bound at a mount point, such
  as `/System/Logs`) the floor is the subtree's own root rather than the
  driver's, so a link stored inside one cannot reach the rest of the volume
  either, and every node a walk reaches has a path under the mount point.
  That totality is what makes canonicalisation (below) able to name every
  result.
- **Following a link never bypasses a permission check.** A spliced
  target's components are authorised exactly as typed ones are: search
  permission is required on every directory the resolution traverses,
  whoever supplied its name.
- **A link is not byte-readable.** Its target is reached only through
  `readlink`; asking an open for byte access to something that really
  is a link is refused with `Errno::LinkLoop`, and `FilesystemWrite::create`
  refuses `NodeKind::Symlink` — a link is created only by `create_link`,
  which carries the target.
- **A target is stored verbatim and never resolved at creation.** It is
  data, so creating a link authorises only the right to add a name in the
  link's own parent and grants no authority over what it names; authority
  is decided at each later use, per component. Its *grammar* is still
  checked before it is stored, so a target this resolver could never walk
  is refused rather than written.
- **A format with no link object type refuses rather than approximating
  one.** `FilesystemRead::read_link` and `FilesystemWrite::create_link`
  default to `DriverError::Unsupported`, which surfaces as
  `VfsError::NotSupported` — never a regular file whose contents merely
  look like a path.

### Which operations follow a final link

The posture is a property of the operation, not a default, so each one names
it. A walk reports the **place** its final name occupies — the directory
holding the name and the name itself — because a driver's name mutations are
keyed `(dir, name)`; under `Follow` that is the *target's* place, which is
what makes an `O_CREAT` reach the target rather than the link. A byte write is
keyed by node and goes to the node the walk resolved, under the same hold of
the driver's lock.

| Operation | Final link | Why |
|---|---|---|
| `read`, `write`, `truncate`, truncate-on-open, append | followed | POSIX acts on the file the link names, and the target's own record decides whether its bytes may be written. |
| `open` | per the descriptor | `FinalLink::for_open` fixes it once from `OpenFlags::NO_FOLLOW`; a `stat` or a listing through the descriptor acts on the node the open pinned. |
| `open` with `CREATE` | followed | Creating through a *dangling* link creates the file the link names, as POSIX specifies, rather than reporting the link's own name as taken. |
| `open` with `CREATE` and `EXCLUSIVE` | kept | The name is taken, as POSIX requires, so a link planted where a program creates exclusively cannot turn the create onto the file it names. `CREATE`, `DIRECTORY` and `EXCLUSIVE` together make a directory and answer it open, with no byte access. |
| `mkdir` | kept | POSIX `mkdir` over a link is `AlreadyExists`, live target or dangling. |
| `symlink` | kept | A new link never replaces an existing name. |
| `unlink`, `rmdir`, `rename` (both ends) | kept | The call is about the name as typed; removing or moving a link never touches what it names. |
| `readlink` | kept | The call is about the link. |
| `link` (existing name) | per the call | An empty `LinkFlags` keeps it, so the *link* gains the second name and one planted on the way cannot redirect it onto an object the caller never spelled (POSIX `link()`); `LinkFlags::FOLLOW` follows it, so its target does instead (`linkat(AT_SYMLINK_FOLLOW)`, what `ln -L` asks for). |
| `link` (new name) | kept | A name being created; a create never replaces an existing name. |
| `chmod`, `chown`, `access` | per the call | Followed, as POSIX applies these to the target, unless the call asks about the link itself. |
| `chdir` | followed | A working directory is a directory. |
| extended attributes | per the descriptor | Reached only through a descriptor, so its open decided. |


## Canonicalisation

Canonicalisation answers the **one** path that names what a path resolves
to: every symbolic link followed, every `..` applied to the nodes the walk
really traversed, and the answer spelled in the caller's own namespace — the
mount point of the projection it ended in followed by the canonical
remainder. A path relative to a working directory or a directory descriptor
is spelled from that directory's canonical path now, however it has moved
since it was entered: the label it was entered by while that still reaches
it without following a link, otherwise the names found climbing from it to
its projection's floor, each in its parent's listing, which the caller must
be able to read. `getcwd` answers the same for the working directory itself
(`FilesystemService::canonical_of`), and a directory no name reaches any more
is `VfsError::NotFound`.
It is the same walk every other operation uses, so a caller cannot obtain a
path the kernel would resolve elsewhere. `fs_realpath` (116) exposes it, and
`readlink -f`/`-e`/`-m` and `ln -r` are its userland consumers; a tool must
not canonicalise for itself, because a copy that disagreed by one rule would
print a path the kernel resolves differently.

`RealpathMode` chooses how much of the path must exist, and nothing else:

| Mode | Requirement | GNU switch |
| --- | --- | --- |
| `Existing` | every component exists | `readlink -e` |
| `Final` | every component but the last | `readlink -f` |
| `Missing` | none need exist | `readlink -m` |

A component that does not exist is carried into the answer unchanged, and a
`..` below the deepest existing node pops that carried tail — the only
reading available where no node exists to ascend from. Nothing above the
deepest existing node is looked up, so `Missing` never leaks existence past
a permission check: search permission on each traversed directory is proven
*before* the child is looked up, exactly as for any other walk.

The answer is always a path this VFS would accept back — at most
`MAX_PATH_COMPONENTS` components and `FS_PATH_MAX` bytes, else
`VfsError::InvalidPath` — so a caller can feed it straight into another
call without being refused for spelling.

## Per-format link support

| Format | Reads a symbolic link | Creates a symbolic link | Hard links |
|---|---|---|---|
| ARXFS  | yes — inode kind `3`, target stored as node data (`arxfs-spec.md` §20) | yes | creates them; `nlink` maintained per inode and storage freed only at zero (`arxfs-spec.md` §20.5) |
| ext4   | yes — both the fast (inline `i_block`) and slow (block-backed) spellings | no; this driver reads foreign links but does not author them | reports `i_links_count` and honours it on unlink; authors none |
| FAT32  | no — the format has no link object type | no | no — a directory entry *is* the file's identity |
| ADFS   | no — the format has no link object type | no | no — the format has no such object |

A "no" is `VfsError::NotSupported`: the permanent format (or driver) limit a
caller can tell apart from a structural refusal, never a substituted file.

## Hard links

A node may carry more than one name. Each is an ordinary directory entry
reaching one inode, so a write through either is visible through the other,
and the node's storage is released only when the **last** name is unlinked
and no hold remains ([held nodes](#held-nodes)) — the count that decides it is
the format's own (`NodeInfo::nlink`), carried up to `FileStat` and printed by
`ls -l`, never derived by the VFS, which could not count names without walking
every directory on the volume.

- **A directory is refused, in the VFS rather than per format.** The tree
  staying a tree is what makes physical `..` resolution well-defined, so
  `VfsError::IsADirectory` answers a directory operand on every backing.
- **A pair that crosses a mount is refused.** A directory entry addresses an
  inode in its own backing, so both paths must resolve under one mounted
  volume; `VfsError::CrossVolume` is the same refusal a rename gives, for the
  same reason, through the same check.
- **The count is a fixed format bound, not a capacity.** A create that would
  overflow it fails closed with `VfsError::TooManyLinks` rather than wrapping
  a count whose zero would free storage a live name still reaches.
- **A second name confers no authority.** It is authorised exactly as a
  create in its own parent (search plus write); nothing further is required
  of the caller against the node, and nothing further is granted.
- **A format that keeps no count answers `1`.** Such a format also has no
  second-name object, so one name is the whole truth rather than a floor
  (`NodeInfo::SINGLE_NAME`, the one definition every such driver reads).

## Held nodes

A driver holds a node for the kernel through `FilesystemRead::pin` until the
matching `unpin` ([driver traits](../abi/driver_traits.md#filesystem)). A held
node that loses its last name keeps its data and takes byte I/O, `node_info`
with `nlink` 0, and security and attribute changes until its last `unpin`
frees it; a held directory that loses its name takes no entries and lists
nothing. No hold survives a crash, so each format finishes or leaks the
deletion it deferred:

| Format | A held node that loses its last name | After a crash |
|---|---|---|
| ARXFS | stays in the pending-delete set, which its drain skips while held; reclaimed at the last unpin | the next writable mount drains the set (`arxfs-spec.md` §14) |
| ext4 | joins the orphan chain in the journalled unlink; released at the last unpin | mount-time orphan recovery frees it |
| FAT32 | its entry is deleted and its cluster chain kept under a token until the last unpin | the chain is lost to the format's checker |
| ADFS | its fragments are kept under a token until the last unpin | the fragments are lost to the format's checker |
| UDF | its file entry and extents are kept at link count 0 until the last unpin | the space is lost to the format's checker |
| ISO 9660 | read-only, so a hold keeps no state | — |

FAT32 and ADFS answer a pinned node under a token, so they declare
`NodeIdentity::ContentDerived`; the others keep a node's id under a hold.

## Open file descriptions

A descriptor of a volume's node holds a pin on it (`kernel/core/src/fs/node.rs`,
`plans/OPENFILE.md`): the `NodeRef`, the projection it was opened through, the
identity its byte operations run under, and the path it was opened as, which
is a label and is never resolved. An open walks once, creates what it was
asked to, reserves the pin's release slot on its volume's registration
(`VolumeNodes`) and pins, all under one hold of the driver's lock.

Nothing after the open looks at a path. A read, write, truncate, punch, seek,
stat, listing or sync runs on the pinned id (`HeldNode`) under the driver's
lock, once the projection is still in the mount table and the registration
current — else `Stale` — and judges the node as [permissions](./permissions.md#descriptors)
describes. A rename of the node or of any directory above it moves nothing a
descriptor holds, a new node made at its old name is another node, and a node
that loses its last name reads, writes and stats on with `nlink` 0 until the
last descriptor lets it go. A held directory's listing shows the projections
attached beneath it only when it is its projection's floor: every projection
sits directly beneath another's floor, and a runtime volume is attached only
directly beneath the catalog, so no other directory has any.

A projection is fixed once made: backing one replaces it, and one leaving the
table is withdrawn, so its descriptors answer `Stale`. A registration is never
reused: detach, retraction, recovery and a clean surprise removal retire it, so
a descriptor of a volume that left never reaches one attached under the same
name since; the stand-in for a volume that vanished with work unpublished
takes over its registration, so its descriptors answer the device error while
it is away.

A pin's last reference may drop anywhere, so the drop only queues its release
— one spin lock, no allocation — and wakes the write-back flusher unless a
drop since its last pass already has. A close tells the driver at once when
its lock is free; the flusher's pass tells it of whatever else was queued
before it publishes.

## Content generations

`FileStat::content_gen` and every directory record's `content_gen` name the
version of a node's data, carried up from the driver's `NodeInfo::content_gen`
unchanged. A volume never hands one out twice, so a node's identity and its
generation name exactly one version of its content — what a cache of something
derived from a file (a thumbnail, an index) keys on to be exact rather than
merely likely. ARXFS keeps one for every file and link (`arxfs-spec.md` §13,
§14); ext4, FAT32 and ADFS keep none and report `0`, which a consumer treats as
no exact version at all.

[`Filesystem`]: ../abi/driver_traits.md
[`Metadata`]: ./permissions.md
[`Credentials`]: ./permissions.md
[`Path`]: ./overview.md
[`VfsError`]: ./overview.md
