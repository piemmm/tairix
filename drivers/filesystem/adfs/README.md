# `tairix-drv-fs-adfs` — Acorn ADFS filesystem driver (read/write)

Reads and writes Acorn ADFS / RISC OS `FileCore` volumes behind any
`tairix_abi::driver::block::Block` device and exposes them through the
versioned `tairix_abi::driver::filesystem::FilesystemRead`,
`FilesystemWrite`, `FilesystemAttrs`, and `FilesystemStats` traits. The frozen `Filesystem` trait carries only
`mount`/`unmount`, so each I/O surface is a versioned trait, never a
widening of a shipped one.

## Supported volumes

| Variant | Map | Directories | Size |
|---------|-----|-------------|------|
| S / M / L | old (sectors 0–1) | 1280-byte `Hugo` (47 entries) | 160/320/640 KiB |
| D | old | 2048-byte `Hugo`/`Nick` (77 entries) | 800 KiB |
| E | new (single zone) | 2048-byte `Nick` | 800 KiB |
| E+ | new | big (`SBPr`/`oven`, 255-byte names) | 800 KiB |
| F | new (multi-zone, boot block) | 2048-byte `Nick` | 1600 KiB |
| F+ | new (multi-zone, boot block) | big | 1600 KiB |
| Hard disc | new (boot block, derived zones) | fixed or big | device-sized |

`Adfs::open` identifies the variant by probing, in order: the
checksummed boot block at `0xC00` (F-class and hard discs), a bare
disc record at byte 4 (E-class), and finally the checksummed old
free-space map with the `Hugo`/`Nick` root marker (S/M/L/D). Every
checksummed structure — old-map sector sums, the boot-block sum, each
map zone's check byte, the map-wide cross-check, and every directory's
check byte and sequence pair — is verified on the way in and rewritten
on every mutation; a mismatch refuses the volume or directory
(`BadMagic`), never "best effort". `Adfs::format` lays out an empty
volume of any variant (floppies use the authentic `FileCore` geometry;
hard-disc zone counts are derived from the device size).

## RISC OS metadata

Load/exec addresses, the 12-bit filetype, the 40-bit centisecond
datestamp, and the `FileCore` attribute bits are surfaced as the
canonical `acorn.*` attribute keys through the shared `tairix_fsmeta`
Acorn preset (`acorn.loadaddr`, `acorn.execaddr`, `acorn.attr`,
`acorn.filetype`, `acorn.datestamp`), so a copy to `ARXFS` and back is
byte-exact. `NodeInfo::times` reports a typed object's stamp as all
four `Time64` instants (the format stores exactly one); untyped objects,
and the zero stamp of a clock never set, honestly report the epoch. A
new object is typed Data (`&FFD`) and stamped from the host's wall clock,
and a write restamps a typed one. ADFS stores no general-purpose
attributes: writing any other namespace is refused (`Unsupported`),
never silently dropped.

## Allocation behaviour

- **Old map** objects are single contiguous sector runs. Growth first
  consumes free space directly after the run, then relocates the object
  (copy to a fresh run, free the old); freeing merges neighbouring free
  areas. A full free-area table (82 entries, the format's
  "compaction required" condition) fails closed with `NoSpace`.
- **New map** objects are fragment chains. Growth absorbs free map bits
  after the object's last fragment, then appends same-id fragments
  later in lookup scan order (never earlier — order is data), and only
  then relocates. Shrinking trims the boundary fragment and frees the
  rest. Shared fragments (small objects packed by share offset, e.g.
  the E-format root) are honoured: a shared fragment is freed only when
  neither the directory holding the object nor another of its entries uses
  it, the only objects `FileCore` shares one with, and a shared object
  grows by relocating to its own fragment.
- **Directories**: fixed directories never resize (47/77 entries is the
  format's bound, reported as `NoSpace`). Big directories grow in place
  by whole 2048-byte grains up to the format's 4 MiB ceiling; a grown
  root's size is rewritten into the disc record (and boot-block copy).
  Directories never relocate, so directory node ids stay stable. A file's
  id is its address in its parent, or a digest of its name while it has
  no allocation, so a write that allocates or moves it, or a truncation to
  nothing, gives it a new id; writes and truncations take the file's id.

## Pins

An object the kernel pins is reported under a token until its last unpin,
since its own id moves with a write and a directory's carries its parent's
address; the root keeps its own id. A removal deletes a pinned object's
entry but keeps its fragments until then, a file sharing a fragment first
taking one of its own. No pin outlives the mount, and `FileCore` keeps no
orphan record, so a crash with a deleted object still pinned leaves its
fragments to the format's checker (`docs/src/filesystem/adfs.md`, Pins).

## Security

ADFS has no per-inode owner, mode, ACL, or capability gate; those live
in the VFS metadata layer that mounts this driver. The driver makes
**no** permission decisions (the VFS is the policy point; this is raw
structural I/O) — including the `FileCore` `L` (locked) bit, which is
surfaced through `acorn.attr` for the policy layer rather than enforced
here. A create ignores the record it is given, and a new object takes the
access bits RISC OS gives one. All validation fails closed, tree walks are
depth-bounded so a cyclic (corrupt) tree cannot spin the driver, and
`#![forbid(unsafe_code)]` holds for the whole crate.

## Required capabilities

- `CAP_DRV_LOAD` at `register` time.
- The filesystem-trait methods are reached only through the
  `DriverHandle` the host minted at load time, and the VFS only
  delegates writes on a non-`READ_ONLY` mount. The driver runs in user
  space; it does not request `CAP_DRV_KERNEL`.
- The crate's `Run` binary, built with the `program` feature, serves a
  removable volume through `lib/fsserve` (`Served`, binding
  `tairix,volume-adfs`), with `CAP_FS_SERVE` and `CAP_LOG_EMIT`
  (`docs/src/filesystem/served.md`).

## Test surface

`cargo test -p tairix-drv-fs-adfs` formats every variant in memory and
exercises, per variant: format/open round-trips with remount, sorted
listings and case-insensitive lookups, nested directories, sparse
writes, truncate grow/shrink/zero, remove with full space reclaim,
renames (move, replace, cycle refusal, parent-pointer fix-up),
growth past a blocking neighbour (the relocation path), disc-full
fail-closed behaviour, name validation, big-directory growth surviving
remount, the `acorn.*` metadata round-trips, and pins (one token for every
hold, kept through writes and renames, a removed or replaced pinned object
kept until its last unpin, a removed pinned directory taking no entries,
`release_pins`, the fragments a crash leaves). A dedicated corruption
suite damages the old-map checksums, directory markers/check
bytes/sequence numbers, zone checks, the cross-check, the boot block,
big-directory headers/tails/heaps, and entry pointers, asserting the
driver fails closed each time. `tests/fuzz_adfs_mount.rs` is the §19.6
harness (registered with `cargo xtask fuzz`): seed-logged single-byte
sweeps over valid images of each map flavour plus PRNG mutations, with
the invariant that `open`, the read surface and a file driven through a pin
never panic.

## Public surface

The only public *function* is `register`. `Adfs` is a public *type* the
driver host constructs with `Adfs::open` (or `Adfs::format`); the host
reaches into it only through the filesystem traits.
