# TAIRiX Storage Namespaces, Volume Roots, and Aliases

This is the binding TAIRiX storage-namespace specification. It supersedes the
AI-facing design brief `plans/DRIVES.md` (which remains only as source
material) and is binding under `AGENTS.md`. Where an earlier informal
description of the filesystem layout disagrees with this page, this page wins.

The one-line decision this page turns into a contract:

> TAIRiX native storage paths are alias-rooted or ID-rooted. `/` is a
> generated **view**, not the root of storage.

## 1. Purpose and non-goals

TAIRiX storage is a **forest of independently addressable named roots**, not a
single global Unix tree. A path names its root explicitly (by human alias or by
stable ID); the familiar `/System`, `/Users`, `/Storage` layout is a
convenience *view* projected over that forest, never the identity of storage.
This removes the single-root fault-tolerance weakness of the Unix model: a
healthy volume stays reachable by its stable ID even when the `/` view — or the
volume that used to back it — is absent, corrupt, or deliberately hidden.

Non-goals: this page defines only how storage is **named, discovered,
published, and resolved to a root**. It does not define an on-disk filesystem
format (that is `arxfs-spec.md` and the foreign-FS driver pages), and
resolving a name never opens anything or grants authority — open/read/write
still enforce inode permissions, ACLs, capability gates, mount flags, and MAC
policy (§14, §15).

Explicitly rejected: drive letters (`C:`), Windows `D:relative` semantics,
filesystem type as durable identity, a path string acting as a capability
token, `uid = 0` as an authority shortcut, and any re-creation of `/proc`,
`/sys`, `/mnt`, or `/media` under another name (§25).

## 2. Charter relationship and `AGENTS.md` amendments

- **`AGENTS.md` §16.1** names exactly three entries in the default session
  root **view**, backed by the first-class aliases `System:`, `Users:`, and
  `Storage:`. The
  canonical identity of a storage root becomes its root ID (`id::`) or alias
  path, not the `/` view path. The amendment preserves the existing rules the
  brief lists under "preserve": Rust-only, microkernel-leaning drivers,
  mandatory capabilities, mode-bits+ACL+capability permissions, no `/proc` /
  `/sys`, the secure default mount policy, and the signed driver-store model.
- **`AGENTS.md` §16.2 / §16.3** (`/System`, `/Users`, `/Storage`
  contents and mount flags) are **preserved** and reinterpreted as the policy
  of the aliases and view bindings those names project (§14, §15).
- No other charter section changes. In particular §4 (no ambient authority),
  §5 (capabilities), §16.6 (System Information API, not a virtual filesystem),
  and §21 (64-bit time) bind this design unchanged.

Adding a new entry to the default root view, or a new resolver to the closed
`abi-v1` resolver table (§6), remains an `AGENTS.md`-amending, ABI-visible act.

## 3. Terminology

| Term | Meaning |
|---|---|
| **Storage object** | Anything that can hold bytes/blocks: disk, partition, network share, disk image, encrypted container, memory backing, WASM host object. Not necessarily a filesystem. |
| **Filesystem driver** | A driver that interprets an on-disk/remote format (`arxfs`, `fat32`, `ext4`, `iso9660`, `adfs`, …). Implementation detail for normal paths. |
| **Volume root** | The root directory object produced by attaching a filesystem driver to a storage object. Independently addressable; not born under another volume. |
| **Volume ID** | A stable, non-human identifier for a volume root, carried by `lib/abi` as a fixed-size type (§8). |
| **Alias** | A human-facing root binding: a short name mapped to a root or view (`System:`, `Home:`, `Backup:`). Closer to a RISC OS disc name or an Amiga assign than a Unix mountpoint or a drive letter. |
| **Resolver** | The namespace component before `::`; selects how the root selector is interpreted (§6). |
| **View** | A synthetic directory tree assembled from roots, aliases, and service entries. `/` is a view; `Storage:/` is a catalog view. A convenience surface, never canonical identity. |

The word **mount** is not the primary abstraction. The precise verbs are:
`discover` a storage object, `attach` a driver to produce a volume root,
`publish` a root into a namespace, `alias` a root under a human name, and
`project` an alias into a view. "Mount" is retained only as a high-level
operation that may perform attach + publish + alias + view projection — never
"place this device under a directory in `/`".

## 4. Design invariants

1. **Names are not authority.** A path string grants nothing; authority comes
   from capabilities, mode bits, ACLs, manifest grants, and delegated handles.
2. **Storage identity is alias- or ID-rooted**, never the `/` view path.
3. **The failure property (binding):** failure of the `System` volume or the
   default `/` view MUST NOT make an unrelated healthy volume unreachable by
   its stable ID. `open id::<uuid>/path` works in a recovery environment with
   sufficient authority even with no `/` view assembled.
4. **One shared parser.** Exactly one path parser and normalization rule set
   for the whole system (`lib/path`, §12); kernel, shell, GUI, and drivers
   import it — never a second parser (`AGENTS.md` §2.2).
5. **Fail closed** on every ambiguous, missing, unauthorized, or malformed
   input (§11, §22); no production path panics.
6. **Platform-neutral.** No board/SoC/target coupling leaks into this
   generic namespace code (`AGENTS.md` §2.20).

## 5. Path grammar

The user-facing native spelling is `Alias:/path/inside/root`:

```text
System:/Kernel/tairix.rxe
Users:/ian/Documents/design.md
Home:/Documents/spec.md
System:/Applications/Editor.app/Run
Backup:/snapshots/latest
CameraCard:/DCIM/100MEDIA/IMG_0001.RAW
```

The resolver-qualified spellings, expanded from the shorthand or written
explicitly for durable/administrative use:

```text
alias::Home/Documents/file.txt                              # expanded Alias:/…
id::b7f2e4e6-8d7a-4ef8-a13e-d3b84d4e8001/Documents/file.txt # stable, durable
fs::arxfs/System/Kernel/tairix.rxe                         # explicit driver
arxfs::System/Kernel/tairix.rxe                            # driver shorthand
adfs::HardDisc4/Apps/Paint                                  # driver shorthand
```

Grammar rules:

- `Alias:/` is absolute from the alias root; the alias name is **not** a path
  component. The first `/` after the `:` marks the root boundary.
- A leading `Name:` (single colon) **not** followed by `/` is not a path: it is
  either a resource reference (`namespace:selector`, owned by the separate
  resource-alias grammar) or a malformed alias path, and is rejected.
- `::` names a resolver. The resolver set is closed for `abi-v1` (§6).
- A leading `/` is the synthetic view root; a bare `path` is relative and is
  resolved by the caller against a current directory.
- `:` is a reserved structural delimiter, so a rendered canonical path always
  re-parses to the same typed value.

Normalization: collapse repeated separators; `.` is the current directory; `..`
never escapes above the selected root (a leading `..` in a *relative* path is
preserved for the caller to resolve); component spelling is preserved except
where a filesystem declares case-insensitive or normalizing behaviour; NUL is
rejected everywhere; malformed Windows-like or URL-like strings are never
silently reinterpreted.

Invalid / valid examples:

```text
# invalid
C:foo                  # drive-letter relative syntax
Home:Documents/file    # alias path missing its root slash
alias::::Home/file     # malformed resolver syntax
/../System             # cannot escape the view root
Home:/../../System      # cannot escape the Home root

# valid
Home:/Documents/file.txt
alias::Home/Documents/file.txt
id::b7f2e4e6-8d7a-4ef8-a13e-d3b84d4e8001/Documents/file.txt
arxfs::System/Kernel/tairix.rxe
/Users/ian/Documents/file.txt          # valid view path, not canonical identity
```

These are the parser's verdicts (§12), what a displayed or compared path must
spell. The kernel does not normalise a `/`-view or relative path it is asked to
resolve: it takes the steps as spelled (`lib/path::steps`) and applies
each `..` to the directory its walk actually reached, a `..` at the view root
staying there as POSIX specifies (§18). An alias or `id::` path is resolved to
its backing location first, and its own `..` still cannot climb above its
root.

## 6. Resolver table

The resolver set is **closed for `abi-v1`**. Adding a resolver is an
ABI-visible, `AGENTS.md`-amending change (§2).

| Resolver | Purpose | User-facing? | Durable? | Notes |
|---|---|---:|---:|---|
| `alias::` | Resolve a named alias in the current namespace | Yes | Sometimes | Backing for `Alias:/path` |
| `id::` | Resolve a stable volume/root ID | Rarely | Yes | The durable machine form |
| `fs::` | Explicit filesystem-driver resolver | No | No | Admin / recovery / testing |
| `<driver>::` | Shorthand for `fs::<driver>/…` (e.g. `adfs::`) | No | No | Driver-backed access |
| `net::` | Explicit remote-root resolver | Sometimes | Depends | Usually published as an alias |
| `dev::` | Raw device / storage-object resolver | No | No | Capability-gated diagnostics |
| `view::` | Named synthetic-view resolver | No | No | Namespace-manager / debugging |

There is **no** `proc::` or `sys::` resolver: runtime system information is the
System Information API's job (§16, `AGENTS.md` §16.6), never a namespace tree.

## 7. Alias model

**Classes.** *Machine aliases* (`System:`, `Users:`, `Storage:`) are
published by the namespace manager from signed policy; *user/session aliases*
(`Home:`, `Desktop:`) are added by the session manager after login and are
scoped to that session; *volume aliases* name a discovered removable/extra
volume; *service aliases* (`Logs:`, `Settings:`, `Recovery:`, `EFI:`) are
published by the owning service.

**Target types.** An alias may bind to a volume root (by ID), to another alias
(indirection, cycle-checked), or to a synthetic view.

**Scopes.** Machine scope (system-wide, capability-published) and session scope
(per-login). A session alias may shadow a machine alias *for that session's
view only*; it never mutates the machine alias table.

**Persistence.** Persistent alias policy is signed (`System:/Security/Policy`)
and loaded at boot; a session alias is ephemeral. Rebuilding the alias table
from discovered volume metadata must produce only safe, unambiguous aliases.

**Conflicts fail closed.** Two volumes claiming the same label do **not**
produce a silent pick: the ambiguous bare alias is refused (`AliasAmbiguous`),
and the volumes remain reachable by `id::` (and by generated disambiguated
display aliases only if policy permits). Their `/Storage` catalog entries
(§10) are not aliases and are never ambiguous: the volume already holding the
label's name keeps it, and the other is catalogued as `<label>_<fingerprint>`
of its own identity, the same at every attach.

## 8. Volume IDs and durable references

A volume ID is a stable, non-human identifier (UUID-shaped or content-derived),
carried by `lib/abi` as a fixed-size `#[repr(C)]` type under the ABI discipline
of `AGENTS.md` §9 (versioned, hashed, frozen on release). `id::<volume-id>/path`
is the durable form for machine records, boot-critical references, recovery
logs, and anywhere a mutable alias is not stable enough. Time-bearing volume
metadata uses `Time64` (`AGENTS.md` §21), never a 32-bit second count.

## 9. Synthetic root view `/`

`/` is a view assembled from the alias namespace. Its default entries are
exactly three, and each is a projection of a machine alias:

```text
/System   -> System:/
/Users    -> Users:/
/Storage  -> Storage:/     # a catalog/view of published non-core aliases
```

There is no `/Apps` and no `Apps:`: a name outside the three fails closed
(`NotFound`).

A view binding may only **preserve or restrict** the source root's authority
and mount flags; it may never relax `ro` / `noexec` without the explicit
relax capability and an audit record (§15). Creating any
additional default-view entry requires amending `AGENTS.md` §16.1.

## 10. Storage catalog `Storage:`

`Storage:/` is a synthetic catalog of published non-core aliases (removable
media, extra disks, network shares), projected into the `/` view as
`/Storage/<name>`. It is a *view projection only*: `/Storage/backup` is nothing
more than a rendering of a volume root — `Backup:/` once an alias names it — and
`Backup:/file` must open even when the
`Storage:` catalog or the `/` view is absent (§4 invariant 3). `Storage:` is
never a parent volume that other volumes live *inside*: `/Storage` is a mount
with no backing, stored on no volume, so its listing is exactly the attached
volumes and nothing can be stored in it — a mutation of it or of a name in it
is refused `ReadOnly`.

**Enumeration is landed** (`plans/DEVICES.md` D3d): listing a projection's
floor merges the projections sitting directly beneath it into the listing
(`MountTable::direct_children`, consumed by the `fs_readdir` service), and
listing the catalog is that merge alone, so a runtime `/Storage/<name>` mount
appears in `/Storage` as a structural directory entry with the same
`UNIX_EPOCH` stamp any stampless backing reports. The mount points list in
name order, so a batched listing resumes after the last one it returned. Every
projection sits directly beneath the floor of another, and a runtime volume is
attached only directly beneath the catalog, so nothing is ever shown beneath a
runtime volume and no directory but a floor has a projection beneath it. An
attach anywhere else is refused, and so is a withdrawal that would leave a
projection beneath no floor.

## 11. Fail-closed rules

A path operation fails closed when: an alias is missing or ambiguous; a
resolver is unknown; a root ID is unavailable; the caller lacks authority to
resolve the root or to traverse/open a component; a view projection would relax
a source root's policy; a filesystem driver reports lossy semantics the caller
did not explicitly accept; a path attempts to escape its root with `..`; or a
driver-specific resolver would bypass normal policy.

## 12. Path parser (`lib/path`)

The single shared parser is `lib/path` (`tairix-path`): `no_std` + `alloc`,
`#![forbid(unsafe_code)]`, linear-time, non-recursive, fail-closed, with fixed
security bounds (`MAX_PATH_LEN`, `MAX_COMPONENTS`, `MAX_COMPONENT_LEN`,
`MAX_ALIAS_LEN` — untrusted-input bounds, not growable capacities,
`AGENTS.md` §24.4). It turns a string into a typed `Path` (a `Root` plus
normalized components) and never resolves, opens, or checks a capability — a
resolved name is still subject to permissions/ACLs/caps/flags/MAC at open time,
so parsing can never widen authority.

**Implemented today.** `Root` covers the spellings with present consumers: the
synthetic view (`/path`), the alias shorthand (`Alias:/path`), the expanded
internal `alias::Name/path`, relative paths, and the durable
`id::<volume-id>/path` form (`Root::VolumeId`, a typed 16-byte identity
parsed only from the canonical hyphenated lowercase UUID spelling — resolved
by the kernel volume forest, `plans/DEVICES.md` D3a). The remaining
administrative resolver spellings (`fs::`, `<driver>::`, `dev::`, `net::`,
`view::`) are *declined* today (`PathError::UnsupportedResolver`), and a bare
`Name:selector` is declined (`PathError::NotAPath`), because they have no
consumer yet.

**Resolver-stage work (remaining).** As recovery/diagnostic tooling lands,
`lib/path` gains the `fs::` / `<driver>::` / `dev::` / `net::` / `view::`
`Root` variants **in place** (`AGENTS.md` §2.13 — no `v2`, no second parser);
each variant is added by the stage that introduces its caller, not
speculatively (§2.3 / §2.4).

## 13. Boot, discovery, and publishing lifecycle

1. Architecture-specific discovery builds the hardware tree (`AGENTS.md` §18).
2. The bootstrap floor brings up the minimal bus/storage path to reach the
   signed driver store (`AGENTS.md` §18.6).
3. The partition layer (`lib/partition`) discovers storage-object boundaries.
4. Filesystem probes identify candidate volume roots.
5. The volume manager publishes an `id::` root for every valid discovered root.
6. The namespace manager loads signed alias policy, if present.
7. The namespace manager publishes the machine aliases.
8. The session manager adds user/session aliases after login.
9. The synthetic `/` view is assembled from the alias namespace.

**Hotplug.** Insertion publishes an `id::` root (and, if policy permits, a safe
alias); removal fails existing handles per their object semantics, marks the
alias unavailable/removed per policy, logs the event, and invalidates **no**
unrelated root or alias.

## 14. Security and capability model

Resolving a path is at least two capability-checked phases: (1) resolve the root
selector (alias/ID/driver root/network export/device); (2) walk the inner path
and open/operate on the object. Both phases check authority before touching
protected state (`AGENTS.md` §5.4). `uid = 0` is never an authority shortcut.

Capability policy reuses the existing `CAP_FS_MOUNT` where it already fits.
Nothing relaxes a volume's `ro` or `noexec` yet, so the `CAP_FS_MOUNT_RELAX`
the charter names for a relaxation does not exist; it arrives with the first
operation that relaxes. Finer authorities (attaching a driver, publishing a
root, administering persistent aliases, altering view projections, opening a
raw device, recovery-only operations, privileged storage inventory) are
introduced **only with the service that holds and enforces them** — never ahead
of a live holder and enforcement point (`AGENTS.md` §5.2). This spec does not
mint speculative `CAP_*`; each such capability lands in the stage that
implements its subsystem.

**File picker / app sandboxing.** An app must not gain broad alias access from
a mere path string. A GUI picker or shell hands the app a one-shot file/directory
capability (a delegated handle), not ambient access to the alias. Path strings
are for display, logs, recent-file lists, and explicit shell operations — never
capability tokens.

## 15. Permissions, flags, ACLs, and MAC

Every open/read/write/mutate still enforces inode owner/mode/ACL, the optional
per-inode capability requirement (`AGENTS.md` §5.3), MAC policy, and the mount
flags. The flags `ro` and `noexec` attach to **root publications and view
bindings**, not only to Unix-style mountpoints. There are no setuid programs
or device files, so the `nosuid` and `nodev` of a Unix mount have nothing to
restrict and do not exist. A view
binding may only preserve or restrict them: its flags are its volume's plus
what it adds, and no operation widens them, so relaxing a flag has no path
today; one would need an explicit relax capability, minted with the operation
that enforces it, and would be audited (`fs.flags.relax.*`). `noexec` refuses
every program read, which is how a bundle on such a binding is refused at
spawn. A filesystem-backed resolver (`adfs::`, `fat32::`,
`arxfs::`) enforces the identical capability/ACL/flag/MAC/audit rules as
`alias::` / `id::` access to the same object — it is never a policy bypass.

## 16. System Information API integration

Privileged storage inventory (the volume forest, root IDs, published aliases,
health) is exposed **only** through typed System Information API queries
(`AGENTS.md` §16.6), each declaring its required capability. There is no
`/proc`/`/sys` device tree and no namespace resolver that scrapes live system
state (§6). This preserves `AGENTS.md` §16.1's ban on fabricating a `/proc`.

## 17. Installer defaults

The installer (`AGENTS.md` §11) creates the required machine aliases
(`System:`, `Users:`, `Storage:`) and the default `/` view containing
exactly those three projected entries. Expert mode may not introduce a legacy
POSIX top-level name into the default root view (`AGENTS.md` §11, §16.1), and
`/Storage/<name>` entries are always projections of independent aliases, never
canonical mountpoints. Single-volume and multi-volume installs both produce the
same alias-first model; a multi-volume install publishes each volume as its own
`id::` root plus its policy-chosen alias.

## 18. Shell and GUI behaviour

The shell displays and consumes alias paths (`cd`, prompt, word/tilde expansion,
completion) through `lib/path` — never a fixed top-level directory set and never
a second parser. `cd` changes a per-process working directory, which is the
directory itself and not a path: `FilesystemService::chdir` answers it pinned
(`Cwd`) once it is a directory its caller may search, so it survives a rename,
a relative path is walked from it, and `..` from it is its physical parent —
at a volume's floor, the catalog the volume is shown in. `getcwd` answers its
canonical path now (`FilesystemService::canonical_of`), and a removed working
directory is `NotFound`. A directory descriptor is a start in the same way. The
file manager shows the alias
forest (`System`, `Home`, `Storage`, plus published volume aliases);
recent-file lists store durable `id::` references so they survive alias
churn.

## 19. Links and cross-root references

Hardlinks are within a single root only. A symbolic link resolves within the
projection that shows it: its `..` stops at that projection's floor, an
absolute target restarts there, and a name it supplies that a projection
beneath that floor is shown under is not found, neither crossing into that
projection nor reaching the directory it covers. A **cross-root**
symbolic link (target names another alias/ID) is resolved only with authority
over the target root and is subject to the same two-phase capability check
(§14); a cycle across views fails closed (`ViewCycle`).

## 20. Foreign-filesystem behaviour

Reading a foreign volume (ext4, FAT32, ADFS, …) is interoperability with the
outside world, not TAIRiX self-compatibility (`AGENTS.md` §2.13). A foreign
driver declares its feature limits (permissions/ACL support, stable file IDs,
label uniqueness/mutability, and timestamp range/precision/representability,
`AGENTS.md` §21) through the filesystem capability API. A weak foreign
filesystem must not weaken TAIRiX security: if it cannot store TAIRiX ACLs or
capabilities, its root publication applies a policy wrapper and defaults to
restrictive flags. Narrowing a `Time64` to a foreign field is checked and fails
with `TimestampOutOfRange` rather than silently truncating (§22).

**The ownerless-format policy wrapper is landed** (`plans/DEVICES.md` D3d):
a runtime-attached FAT32 volume is mounted under the kernel's storage-group
identity map (`tairix_kernel::volume_policy::GroupMappedFs`) — every node
appears owned by the system user and the well-known `storage` group
(`tairix_users::STORAGE_GROUP`, resolved **by name** from the loaded group
registry at root unlock), directories `rwxrwxr-x` and files `rw-rw-r--`, so
any logged-in member reads and writes the medium while security-record
stores stay refused (the format cannot hold one). A registry without the
group leaves such a volume restrictively system-owned.

**A format that records ownership is served with it** — ext2/3/4 and ARXFS,
removable or not. Every node presents the uid, gid, mode and ACL it stores;
nothing maps, ignores or takes over ownership, and no mount option or
capability changes that. A node made there carries its creator's uid and
primary gid, and `chmod` and `chown` follow the owner and `CAP_FS_CHOWN` rules
of any volume. So a volume made elsewhere keeps its owners' rules: a
privileged owner's node stays out of a lesser user's reach, which loses access
and grants none, and a user who is not the owner of a volume's root cannot
make anything at its top level. The file manager states such a refusal and the
item it concerned.

## 21. ABI and crate ownership

| Home | Owns |
|---|---|
| `lib/path` (`tairix-path`) | The `no_std` path parser and normalizer (§12). |
| `lib/abi` (`fs` / `storage` / `sysinfo` types) | ABI-visible filesystem/path/root/volume-metadata types + the descriptor-producing open-a-path ABI + storage sysinfo queries (§16). All `Time64` where time is stored/reported. |
| `lib/caps` | Capability IDs and checks. |
| `userland/system/devmgr` | Storage-driver autoload integration. |
| `userland/system/installer` | Default alias-policy creation (§17). |
| `userland/shell/elsh` | `Alias:/` consumption and display (§18) — via `lib/path`, no private parser. |

The **descriptor-producing open-a-path ABI** — the syscall that opens a
resolved path to a new file descriptor (consumed by, not invented in,
`tairix_rt::io`) — is landed: `fs_open` and its `fs_close`/`fs_read`/
`fs_write`/`fs_readdir`/`fs_stat`/`fs_truncate`/`fs_sync`/`fs_mkdir`/
`fs_unlink`/`fs_rename` family, all gated by `CAP_FS_ACCESS` and fail-closed,
and the ergonomic I/O layer opens files over the same `Read`/`Write`
vocabulary. Every path-taking call resolves at the single kernel entry point
through `lib/path`, and **machine-alias resolution** is wired there:
`Alias:/path` and the expanded `alias::Name/path` resolve for the three machine
aliases, which are the canonical roots the `/` view projects as `/<Name>`
(`kernel/core::fs::resolve_machine_alias`, derived from the one root-view
definition, `tairix_abi::ROOT_VIEW`, so the view and the alias namespace
cannot drift). **Durable `id::`
resolution is wired at the same entry point** (`plans/DEVICES.md` D3a): the
kernel volume forest (`kernel/core::fs::volumes::VolumeForest`, installed via
`BootInfo::with_volumes`) maps each mounted volume's stable identity — the
ARXFS per-volume UUID, published by the boot mount/unlock paths with the
audited `fs.root.publish.{allow,deny}` events — to the `/`-view location its
root backs, so `id::<volume-id>/path` opens the same object under the same
permissions, never a policy bypass, and an unpublished identity fails closed
`NotFound`. **Runtime attach and unpublish are landed** (`plans/DEVICES.md`
D3b): the `CAP_FS_MOUNT`-gated, audited `volume_attach` / `volume_detach`
syscalls attach a hot-pluggable block source (the kernel blkio client over a
served block-service endpoint + shared window), and the volume's format is
served by its user-space driver ([Volumes served from user space](./served.md)),
whose root is mounted at `/Storage/<name>` with the removable-media flags and
published/withdrawn as an `id::` root through the same forest — a detach
flushes first and fails closed rather than discarding uncommitted data. A
catalog name another volume holds is answered `AlreadyExists` before the
volume is judged, so the volume manager's next candidate name costs nothing; a
volume already attached under another name is `Busy`, which no other name
answers.
**Automount
policy, catalog enumeration, and the mount-policy identity map are landed**
(`plans/DEVICES.md` D3c/D3d): the `volmgr` policy driver probes and attaches
recognised volumes under names derived from each volume alone — its label,
else its transport and identity fingerprint — `/Storage` listings enumerate
the published runtime mounts (§10), and an ownerless foreign format mounts
under the storage-group identity map (§20). Alias policy and the `fs::`
resolver remain future increments; machine aliases then rebind from the
single root's subtrees to independent `id::` volume roots without changing
the resolver contract.

## 22. Error model

Typed, fail-closed errors (no production path panics on malformed input, a
missing alias, an absent device, an I/O error, or an unsupported feature):

```text
UnknownResolver   MalformedPath        InvalidAliasName
AliasNotFound     AliasAmbiguous       AliasUnavailable
RootIdNotFound    RootUnavailable      PermissionDenied
CapabilityRequired ViewUnavailable     ViewCycle
PathEscapesRoot   FilesystemDriverUnavailable
FilesystemFeatureUnsupported           ForeignFilesystemLimit
TimestampOutOfRange DeviceUnavailable  RawDeviceAccessDenied
PolicyWouldRelaxFlags
```

`lib/path`'s parse-time `PathError` (malformed, over-long, escaping, not-a-path,
unsupported-resolver) is the *spelling* subset of this model; the resolve/open
errors above are surfaced by the storage/filesystem subsystems that add the
open-a-path ABI (§21).

A vanished device answers the *device* error, never the *feature* one, on
**every** operation. `FilesystemFeatureUnsupported` is a permanent claim about
the format ("this volume has no symbolic links"); `DeviceUnavailable` is a
condition a re-insert can clear, and a caller must be able to tell them apart.
So the stand-in a removal installs under the volume's handle writes out every
method of every facet it claims — including the ones the driver ABI defaults —
because inheriting a default would answer the wrong one of these two.

A descriptor holds its volume's registration, never its name: one whose volume
was detached, retracted or re-attached since it opened answers `Stale`, and
never reaches a volume attached at the same name since; one of a volume held
unavailable answers the device error until a recovery or a verified re-insert
gives the volume a fresh registration. A plain detach is refused `Busy` while
a descriptor holds a node of the volume; a force detach releases every pin,
completing each deferred deletion, and revokes them.

A removal applies to every volume on the device, a partitioned stick carrying
several over one endpoint. A volume whose driver holds writes it acknowledged
but never gave the device is recorded `lost`, whatever its journal holds. The
device's block cache lets go of the removed medium's blocks at once when
nothing is using it, and otherwise goes with the device at the next attach or
detach; a volume held unavailable keeps its journal, never the device.

## 23. Audit events

Security-relevant namespace decisions are logged through `lib/log` with stable
event IDs (`AGENTS.md` §5.4, §19.4): `fs.root.{discovered,attached}`,
`fs.root.publish.{allow,deny}`, `fs.alias.create.{allow,deny}`,
`fs.alias.remove.{allow,deny}`, `fs.alias.resolve.deny`,
`fs.view.bind.{allow,deny}`, `fs.flags.relax.{allow,deny}`,
`fs.raw_device.open.{allow,deny}`, `fs.hotplug.root_{added,removed}`,
`fs.hotplug.surprise_removal.{clean,dirty,lost}` (a vanished device with
nothing uncommitted / with its uncommitted writes retained / after retention
was abandoned), `fs.hotplug.force_unmount` (a volume force-unmounted, its
retained uncommitted writes deliberately discarded — the event carries the
discarded byte count and the reason a clean commit was impossible),
`fs.hotplug.reinsert_replayed` (a re-inserted volume proven unmutated, its
retained writes replayed and committed — carries the replayed byte count),
`fs.hotplug.reinsert_conflict` (a re-inserted volume whose non-mutation could
not be proven, mounted fresh and read-only with the retained set kept —
carries the refusing cause and the retained byte count),
`fs.serve.{awaiting,resumed,refused,lost}` (a removable volume's user-space
driver awaited, serving again, refused, or gone — see
[Volumes served from user space](./served.md); `fs.hotplug.root_added` is then
recorded when its driver first serves it), and
`fs.conflict.alias_ambiguous`. No secret, key,
capability-token value, or private path content beyond the audit subsystem's
policy set is logged.

## 24. Required tests

- **Parser** (landed in `lib/path`, incl. the `fuzz_path` round-trip harness):
  `Home:/…` and `alias::Home/…` parse to the same root+components; `id::<uuid>/…`
  and `adfs::HardDisc4/…` are handled per §12; `Home:Documents/…`, `C:foo`, and
  NUL-bearing paths are rejected; `Home:/../../System` cannot escape the root.
- **Namespace** (resolver stage): alias lookup needs authority; missing/ambiguous
  aliases fail closed; session overrides do not mutate the machine table; a
  sandboxed process sees only delegated aliases; a multi-target alias write
  needs an explicit target.
- **Fault tolerance** (resolver stage): a volume opens by `id::` with the `/`
  view or `Storage:` unavailable; corruption of `System:` does not hide a
  healthy `Backup:` from an authorized recovery environment.
- **Security** (resolver stage): publish/alias/view mutation require capability;
  a view binding cannot relax flags without the relax capability; driver-backed
  resolvers do not bypass ACLs/caps/flags; raw `dev::` is denied without
  authority; inventory is not reachable via `/proc`/`/sys`.
- **Installer / hotplug** (resolver + installer stages): default install creates
  the aliases and exactly the three view entries; expert mode refuses legacy
  names; insertion publishes an `id::` root; removal invalidates only that root.

## 25. Rejected designs

Option A (keep §16 literally, aliases layered on top) is rejected: it preserves
the Unix single-root failure model. The forbidden outcomes of `AGENTS.md` and
the brief are binding here: storage identity dependent on one `/` tree; volumes
canonical only as `/Storage/<volume>`; `/mnt` / `/media` under another name;
drive letters or `D:relative` semantics; filesystem type as durable identity;
`adfs::` / `fat32::` / `arxfs::` bypassing policy; a path string as a
capability token; `uid = 0` bypassing namespace checks; `/proc`/`/sys` recreated
under any resolver; a second path parser; C code or hand-written headers;
target-specific behaviour in generic code; or a new default-view entry without
amending `AGENTS.md`.
