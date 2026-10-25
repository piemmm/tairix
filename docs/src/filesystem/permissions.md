# Filesystem permissions

The VFS enforces the `AGENTS.md` §5.3 model: **POSIX mode bits, plus
ACLs, plus a per-inode capability gate**. Every operation routes its
decision through one function (`Metadata::authorize`), which fails closed
and **never branches on `uid == 0`** — authority comes from capability
grants, ACL entries, and mode bits, never from a magic user id
(`AGENTS.md` §5.1).

## Inode metadata

Each inode stores:

- `owner` (uid) and `group` (gid),
- POSIX `mode` bits (owner/group/other `rwx`, plus `setuid`/`setgid`/
  sticky),
- an optional `required_cap` capability gate,
- an `acl`: a list of explicit allow/deny entries, each naming a user or
  group, an access right, and a decision.

## The three layers, in order

A request for `Read`, `Write`, or `Execute` access is decided as follows:

1. **Capability gate.** If the inode declares a `required_cap` and the
   caller does not hold it, access is denied *regardless of the mode
   bits*. A file marked `CAP_AUDIT_READ` is unreadable at mode `0644` by a
   caller without that capability.
2. **ACL.** Among entries matching the caller (by uid, or by membership in
   a named group) for the requested access: an explicit **deny** wins; an
   explicit **allow** grants it. With no matching entry, the decision falls
   through to the mode bits.
3. **Mode bits.** The owner / owning-group / other triad is selected by the
   caller's identity, and the requested `rwx` bit is checked.

Only the inode's **owner** may change its capability gate
(`Vfs::set_required_cap`).

## The gate guards the name, not only the node

Adding, removing, and renaming a directory's entries is authorised against
that **directory's** write permission, not against each entry. On its own
that would make a capability gate bypassable by anyone who can write the
holding directory: unlink or rename the gated node aside, create an ungated
one of the same name, and the service that reaches the tree only by holding
the capability walks into the replacement instead. A same-parent rename is
the sharpest case, because POSIX authorises the moved node itself only when
the two parents differ.

So a `required_cap` node's *name* carries the gate too. `unlink`, `rmdir`,
and `rename` — at either end, since replacing a name destroys its occupant —
demand the capability, exactly as a mode or ownership change on the node
already does. A hard link needs no separate rule: a second name shares the
one inode and therefore the one security record, so it confers no reach the
first name did not.

## Sticky directories

In a directory carrying the sticky bit (`0o1000`), write permission lets a
principal add names but not take away anyone else's: `unlink`, `rmdir` and
`rename` — again at either end — succeed only for the entry's owner or the
directory's owner. A shared drop folder (`0o1777`) is the case it exists
for. No capability overrides it; a `CAP_FS_CHOWN` holder who must remove
another's entry takes ownership of it first, which is audited. Both rules are
one check, `Metadata::admits_name_removal`.

## What a node is born with

A node is born with its whole security record, in the creating call. Its
owner is its creator. Its permission bits are the mode the create asked for
(`fs_open` with `CREATE`, `fs_mkdir`) less the creating process's file mode
creation mask; a symbolic link's are always `0o777`, since a link's own mode
never gates access. The mask withholds only the `rwx` triads, never the
setuid, setgid or sticky bit.

Its group is its creator's own, except in a **setgid directory**
(`0o2000`): that hands its group to everything made in it, and its setgid
bit to a new directory, so a shared folder keeps its group however deep it
grows. A file keeps a setgid bit it asked for only when its creator belongs
to that group.

The mask is per process: PID 1 starts with `0o022`, a child inherits its
spawner's, and `fs_umask` sets it, answering the one it replaces. It needs no
capability, since it decides only what the process's own creates are born
with.

## Where the metadata comes from

For the in-RAM tree the `Metadata` above is held directly on the inode.
For a [driver-backed mount](./overview.md) it is the node's own record, read
through the driver's `FilesystemSecurity` surface — the full per-inode record
[arxfs](./arxfs.md) stores, or what a format that stores less reports for
each node — and translated by `Metadata::from_node_security`. Each
grant-only driver ACL entry (a `subject` plus an `rwx` triad) expands into
one **allow** ACL entry per bit it grants; the driver surface carries no
explicit deny. Either way the resulting `Metadata` feeds the *same*
`authorize` decision above.

A driver whose format keeps **POSIX.1e** access ACLs, such as
[ext4](./ext4.md), reports its record under `AclModel::Posix`, and the ACL
layer then decides the way the system that wrote the ACL would, in place of
steps 2 and 3: the owner by the owner triad alone; a named user by its entry
under the mask; a member of the owning group or of any named group by the
union of the matching entries — the owning group's own entry among them —
under the mask, refused if they do not grant it and never falling through
to other; anyone else by the other triad. An entry granting nothing still
names its principal, so a user or group listed with no access is refused,
never judged as anyone else. The capability gate still comes first.

## Traversal

Resolving a path requires **search (execute) permission on every directory
descended through**, mirroring POSIX — including the directory each `..`
leaves, and the working directory or directory descriptor a relative path
starts at, asked as the caller on every call. A directory is never granted,
so a directory descriptor's authority is always its holder's. `stat`-style
`metadata` therefore needs search permission along the path but no permission
on the target itself; `read` additionally needs read permission on the file;
`mkdir`, `create_file`, `symlink`, and `remove` need write permission on the
*parent* directory.

A **symbolic link** changes nothing here: search permission is required on
every directory the resolution traverses whether the caller spelled that
component or a link's target supplied it, so following a link can never
reach a directory the caller could not have walked itself. Reading a link's
own target (`readlink`) needs search permission on the way to it and none
on the link, mirroring POSIX.

## Descriptors

A descriptor holds the node its open pinned, not a path
([overview](./overview.md#open-file-descriptions)). The open is judged as any
resolution is, and decides the access the descriptor carries. Every byte
operation through it after that — read, write, truncate, punch, seek, listing,
lock, a mapped page's fault — is judged again on the node alone, under the
descriptor's authority: the caller's live identity for a descriptor it opened,
so a capability narrowed while it is open stops working at once, and the
identity captured when one was conferred at spawn or granted. `stat` and `sync`
through a descriptor ask only the filesystem capability, as POSIX's `fstat` and
`fsync` ask no permission.

The capability gate binds every call. The mode and the ACL bind every call too,
except that the node's owner, through a descriptor that was not granted, passes
them (`Metadata::authorize_held`): the owner could restore any bit, and POSIX
requires `open(O_CREAT, 0444)` then `write` to succeed. A grant has no such
exemption, so the owner's `chmod` revokes one. Search permission above a held
node is never asked again: a descriptor is not a name.

A node's extended attributes are reached only through a descriptor, judged as
its bytes are: read permission to read or list them, write permission to
change them. A mode or owner change through a descriptor is judged as a change
by path is, and is never made through a grant, whose holder could not restore
what it changed.

## Asking about access

`fs_access` answers whether the caller may have a node as it asks — read,
write, run, or with nothing asked whether the node exists — as the caller:
the node's mode, ACL and capability gate under the caller's own identity,
then the projection's flags, so a write where nothing may be written is
`ReadOnly` and running a regular file on a `noexec` projection is
`PermissionDenied`. It changes nothing, and a later operation is judged
afresh.

## Errors

Permission failures return `VfsError::PermissionDenied`. A write blocked by
a read-only mount returns `VfsError::ReadOnly`. `VfsError::to_errno` maps
them to the stable user/kernel `Errno` at the syscall boundary as
`PermissionDenied` and `ReadOnly` (the `EROFS` analogue, "read-only file
system"), so a tool can tell a node it may not change from a volume nothing
may change. Creating a top-level directory — including
a legacy POSIX name the OS itself never authors — is governed by ordinary
write permission on the root directory; the VFS reserves no error for the
name itself (see the [layout page](./layout.md)).
