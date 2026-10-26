# ADFS driver

`drivers/filesystem/adfs` (`tairix-drv-fs-adfs`) is the read/write
interoperability driver for Acorn ADFS / RISC OS `FileCore` volumes. It
attaches a volume behind any `tairix_abi::driver::block::Block` device
and exposes it through the versioned `FilesystemRead`,
`FilesystemWrite`, `FilesystemAttrs`, and `FilesystemStats` traits. A
node's timestamps travel in `NodeInfo::times`, filled by the read
surface (there is no separate timestamp trait).

## Formats

Every ADFS on-disc format is supported, for reading and writing:

- **Old map** (S, M, L, D floppies and old-map hard discs): the
  checksummed free-space map in sectors 0–1, with every object a single
  contiguous run of 256-byte sectors. S/M/L use the 1280-byte `Hugo`
  directory (47 entries); D uses the 2048-byte directory (77 entries).
- **New map** (E, F floppies and new-map hard discs): a multi-zone
  allocation map of variable-length fragments — an `idlen`-bit fragment
  id, zero bits, and a stop bit per fragment, with a per-zone free-space
  list, per-zone check bytes, and a map-wide cross-check. F-class and
  hard-disc volumes carry the checksummed boot block at `0xC00` with the
  embedded disc record; E-class volumes carry the record at the start of
  zone 0. Small objects may share a fragment through the share offset in
  the low byte of an indirect disc address.
- **Big directories** (E+, F+): the variable-length `SBPr`/`oven`
  format with a name heap and 255-byte names, growing in 2048-byte
  grains to the format's 4 MiB ceiling. The root's size lives in the
  disc record and both copies (map zone 0 and the boot block) are kept
  in step when it grows.

The variant is identified by probing (boot block, then bare disc
record, then old map), and every checksum is validated before the
volume is accepted; corruption anywhere refuses the volume or directory
rather than serving suspect data.

## RISC OS metadata

The RISC OS load/exec words are surfaced through the canonical
`acorn.*` attribute keys of the shared metadata registry
([metadata registry](./metadata-registry.md)): `acorn.loadaddr`,
`acorn.execaddr`, `acorn.attr`, and — for filetyped objects —
`acorn.filetype` and `acorn.datestamp`. The conversions live in the
`tairix_fsmeta` Acorn preset, so a copy to `ARXFS` and back reproduces
the native fields byte-for-byte. A typed object's 40-bit centisecond
stamp is reported (widened to `Time64`) as all four node timestamps;
ADFS stores exactly one instant, and an untyped object honestly reports
the epoch rather than a fabricated time. So does RISC OS's own epoch, the
zero stamp: it is the reading of a clock that was never set.

A new file or directory is typed Data (`&FFD`), as RISC OS types a file
created without one, since an untyped object's load and exec words would
name addresses to load and run it at. It is stamped with the host's wall
clock (`VolumeHost::wall_time`, [driver traits](../abi/driver_traits.md)),
to the centisecond. A write or truncate restamps a typed object, and an
untyped one keeps its addresses. The entry is an object's only metadata,
so the stamp rides with the write's own entry update where the write needs
one; an overwrite in place pays one directory write for it. A rename keeps
the stamp. While the clock is unset, or for a time outside the 1900–2248
the stamp holds, the stamp is zero, which reads back as no time.

ADFS has no per-inode owner, mode, ACL, or capability gate: the §5.3
security metadata lives in the VFS layer, and the driver makes no
permission decisions. The `FileCore` attribute bits (including `L`,
locked) are surfaced as data through `acorn.attr` for the policy layer.
They are RISC OS's own access controls, not a store for TAIRiX's record:
a create ignores the record it is given, and a new object takes the bits
RISC OS gives one, owner read and write for a file and owner read for a
directory, locked on the 1280-byte format.

## Allocation

Old-map growth extends a contiguous run in place where free space
follows it and relocates the object otherwise; the 82-entry free-area
table is a format bound and fails closed (`NoSpace`) when fragmentation
exhausts it — the format's own "compaction required" condition. New-map
growth absorbs free map bits after an object's last fragment, appends
same-id fragments later in scan order (lookup concatenates fragments in
scan order, so appending earlier would reorder data), and relocates as
the last resort; shrinking returns fragments to the per-zone free
lists. A shared fragment is freed only when neither the directory
holding the object nor another of its entries uses it: `FileCore` shares a
fragment only between a directory and its files, or files of one
directory.

Fixed directories never resize (their 47/77-entry capacity is a format
bound); big directories grow in place and never relocate, keeping
directory node ids stable.

## Node ids

An object's id names it by its address and the address of the directory
whose entry holds it, so a stat, read or write by id reads that one
directory; the id is also the number `stat` reports. An object with no
allocation has no address, and is named by a digest of its name instead.
A write that gives an empty file its first allocation or moves a file, and
a truncation to nothing, therefore change its id: the driver declares
`NodeIdentity::ContentDerived`, and a caller looks the name up again after
writing. Two empty siblings whose names share a digest share an id, and a
write by it is refused as `Unsupported` rather than guessed. A directory's
id carries its parent's address, and the parent pointer each directory
records gives the parent's own. `parent` finds the directory's entry in its
parent first, so a removed directory answers `NotFound`.

## Pins

An object the kernel pins is reported under a token until its last unpin,
since its own id moves when a write relocates it and a directory's carries
its parent's address. A file's pin finds it again by its directory's address
and its name as `FileCore` folds it, which a rename updates; a directory's
by its address, which a directory keeps for life; the root keeps its own id.
Lookups, listings and `parent` answer the token, while `stat` goes on
reporting the object's own number. Every table a pin is recorded in grows
fallibly, so a hold that cannot be recorded is refused with `OutOfMemory`.

A removal of a pinned object, or a rename replacing it, deletes its entry but
keeps its fragments until the last unpin frees them (`Unpinned::Freed`): the
file reads, writes, truncates and changes its `acorn.*` metadata through its
token, and `stat` reports no name. A file sharing a fragment first takes one
of its own, since a shared fragment is freed with the last entry naming it.
A removed directory a pin holds takes no entries: a lookup in it, its parent
and a creation or move into it answer `NotFound`, and it lists nothing.

No pin outlives the mount, and `FileCore` keeps no orphan record, so a crash
with a deleted object still pinned leaves its fragments allocated and named
by no entry, for the format's own checker to reclaim.

## Listing

Directory entries are kept in case-insensitive sorted order, so an
insertion or removal shifts every entry after it. A listing therefore loads
the directory once per call and resumes by name rather than position: its
cursor carries the index and the volume's write generation, which every
insertion and removal bumps. While the generation is unchanged the index
holds; once it has moved, the listing resumes just past the name it last
returned, by binary search, a place no insertion or removal moves — so an
entry present throughout a listing is listed exactly once.

## Names

Names match ignoring ASCII case, and the driver declares that rule to the VFS.
A rename to a case variant of an entry's own name re-spells it where it stands
— its place in the order is unchanged — and a rename replacing a sibling takes
the spelling asked for.

## Served from user space

On removable media the driver is served from user space ([Volumes served from user space](./served.md)): its `Run`
binary binds `tairix,volume-adfs` and runs with `CAP_FS_SERVE` and
`CAP_LOG_EMIT`, and the volume is mounted under the storage-group identity
map.

## Test surface

Per-variant unit tests cover round-trips (with remount), listings,
lookups, sparse writes, truncation, removal with space-reclaim
verification, renames including replacement and cycle refusal, forced
relocation, disc-full behaviour, name validation, big-directory growth,
the metadata round-trips, and pins: one token for every hold, kept through
writes and renames, a removed or replaced pinned object kept until its last
unpin, a removed pinned directory taking no entries, `release_pins`, and the
fragments a removal leaves allocated when the mount ends. A corruption
suite damages every checksummed structure in turn (old-map sums, directory
check bytes and sequence pairs, zone checks, the cross-check, the boot
block, big-dir headers and heaps, entry pointers) and asserts the driver
fails closed.
`tests/fuzz_adfs_mount.rs` is the fuzz harness registered with
`cargo xtask fuzz`: seed-logged byte sweeps over valid images of each
map flavour plus PRNG mutations, with the invariant that `open`, the read
surface and a file driven through a pin never panic.
