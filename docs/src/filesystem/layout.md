# On-disk layout enforcement

This page mirrors `AGENTS.md` §16. The VFS in `kernel/core::fs` enforces
it; the installer (Stage 8) lays it out.

## Exactly three default root-view entries

The default session root view (`/`) has **exactly three** entries
(`tairix_abi::ROOT_VIEW`):

```
/
├── System/    # All OS-provided files. Read-only at runtime.
├── Users/     # One subdirectory per user account.
└── Storage/   # Catalog/view of published non-core storage roots.
```

These are synthetic view bindings backed by the first-class aliases
`System:`, `Users:`, and `Storage:` — the canonical identity of a
storage root is its root ID or alias path, not the `/` view path
(`AGENTS.md` §16.1; the binding model is the storage-namespace spec,
[Storage namespaces, volume roots, and aliases](./drives.md)). The
single-root VFS this page describes is the *current realization* of that
default `/` view; the forest-of-roots model it projects lands with the
resolver / open-a-path stage tracked in that spec.

`Vfs::with_default_layout` provides exactly these three entries and,
beneath `/System`, the two writable exceptions `Logs` and `Settings` (see
below). Machine-wide applications live in `/System/Applications`; there is
no `/Apps`.

## Legacy POSIX names: the OS never authors them

The legacy POSIX top-level names —

```
etc  home  usr  var  proc  sys  lib  lib64  bin
sbin opt   root tmp  dev   mnt  media run    boot
```

— are names the **OS itself never creates**. `Vfs::with_default_layout`
lays out exactly the permitted entries and nothing else, the image builder
authors only the two of them a volume stores (`tools/mkimage`), and the
installer refuses to lay any legacy name out (`AGENTS.md` §11). No
in-tree component hard-codes one of these paths.

This is a rule the OS keeps to, **not** a structural ban the kernel
imposes on userland. The VFS does **not** police a user's own request:
with write permission on the root directory a caller may `mkdir /etc`
like any other directory — ordinary owner/mode/ACL permission on `/`
governs it, exactly as for a non-legacy name, with no separate
capability and no `VfsError` reserved for the name. Because production
`/` is owned by the system user with a restrictive mode, an unprivileged
user cannot create a top-level entry of *any* name; a privileged one
may.

There is no OS-provided `/proc` and no `/sys`: the OS does not create
them and nothing relies on them. Live system information is exposed
exclusively through the System Information API (`AGENTS.md` §16.6,
Stage 6).

## Read-only `/System`

`/System` is mounted read-only at runtime. Its only writable paths are
`/System/Logs` and `/System/Settings`, which are separate child mounts
flagged `noexec`.

The mount table resolves the **longest mount-point prefix** of a path, so
a child mount shadows its parent:

| Path                     | Covering mount   | Writable? |
| ------------------------ | ---------------- | --------- |
| `/System/Drivers/vesa`   | `/System`        | no        |
| `/System/Logs/boot`      | `/System/Logs`   | yes       |
| `/System/Settings/host`  | `/System/Settings` | yes     |

Because creating or removing an entry mutates the *parent* directory, the
writability of a create/remove is governed by the **parent's** covering
mount. This is why removing the `/System/Logs` mount point itself is
refused (its parent `/System` is read-only), while writing a file *inside*
`/System/Logs` is allowed. A write to a read-only location returns
`VfsError::ReadOnly`.

## Default mount policy

`Vfs::with_default_layout` installs the `AGENTS.md` §16.2 / §16.3 mount
flags. A mount exists only where a different volume, or a narrower policy,
begins; `/Users` is a directory of whatever backs `/`.

| Mount               | Flags        |
| ------------------- | ------------ |
| `/System`           | `ro`         |
| `/System/Logs`      | `noexec`     |
| `/System/Settings`  | `noexec`     |
| `/Storage`          | `ro,noexec`  |

`ro` refuses every change. `noexec` refuses every program read: the spawn
path reads each file of a bundle as a program
(`FilesystemService::read_program`), so a bundle on such a mount is refused
at its first byte, however it is reached — a link resolves inside its own
mount, so none on another volume can point a spawn at it. Reading the same
files as data is unaffected.

A volume's attachment carries its own flags, and every view binding of it
keeps them, narrowing only: a binding's flags are its volume's plus what the
binding adds, so no binding can widen what its volume was attached with.

There are no setuid programs and no device files, so a Unix mount's
`nosuid` and `nodev` have nothing to restrict and do not exist.

## The `Storage:` catalog

`/Storage` is a mount with no backing: the catalog of the volumes attached
beneath it, stored on none of them. It stats and opens as a directory, its
listing is exactly the volumes attached at `/Storage/<name>`, and nothing
can be stored in it — a create, rename, mode change or removal of it or of a
name in it is refused `ReadOnly`, and a name no volume serves is
`NotFound`. A volume is attached beneath it at runtime as its own mount,
`noexec` by default, carrying its own permission template.

## Boot-time volume layering: writable root, read-only `/System` shadow

`with_default_layout` is the in-RAM shape before any disk is mounted. At
boot the production `fs_*` mount table (`kernel/tairix-kernel`'s
`system_mount`) wires two on-disk `ARXFS` volumes into that layout:

- the **writable root volume** (`ARXFSRoot`), plaintext or encrypted, is
  mounted as `/` (`MountTable::back_root`). It is the persistent home of
  `/`, `/Users`, and the writable `/System` exceptions;
- the **read-only, plaintext `/System` volume** (`ARXFSSystem`)
  is mounted *over* it at `/System`, carrying the immutable kernel image,
  drivers, and libraries.

The writable `/System/Logs` and `/System/Settings` are then attached to
the *same* writable root volume (`MountTable::set_backing`), each rebased
onto its own same-named path on that volume, to shadow the read-only
volume. Longest-prefix resolution gives every path exactly one covering
volume:

| Path                    | Resolves to        | Writable? |
| ----------------------- | ------------------ | --------- |
| `/Users/alice/notes`    | `ARXFSRoot` (`/`) | yes       |
| `/System/Drivers/vesa`  | `ARXFSSystem`     | no        |
| `/System/Logs/boot`     | `ARXFSRoot`       | yes       |
| `/Storage/stick/a`      | the attached `stick` volume | as attached |

This is **disjoint sub-mounting**, never a union/overlay "merge" of two
`/System` trees: no path is served by both volumes, so resolution stays
deterministic and fail-closed. Consequently the two volumes carry
**non-overlapping** content — the immutable `/System` subtree lives only
on `ARXFSRoot`'s read-only sibling, and the root volume authors only
the writable-state subtree (`/System/Logs`, `/System/Settings`,
`/System/Security`) plus `/Users` — every root-view entry but the catalog
(`tairix_abi::stored_root_entries`).

Until the root volume is mounted, the writable root driver is not yet
registered and every operation on `/` and its writable subtrees fails
closed (`Errno::NotImplemented`), never a silent fallback to the
read-only `/System`.
