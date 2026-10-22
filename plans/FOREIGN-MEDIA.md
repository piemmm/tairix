# FOREIGN-MEDIA.md — discs and the media other systems write

Binding under `AGENTS.md`. Reading and writing the volumes other systems
master — CD and DVD file systems, UDF on any device — from the drives that
hold them, and copying their files onto ARXFS with nothing lost: a RISC OS
filetype, a Mac type and creator, an Amiga comment, a sparse file's holes.

| Id | Item | Status |
|---|---|---|
| M1 | ISO 9660 reading: ECMA-119 and High Sierra, Joliet, ISO 9660:1999, Rock Ridge with zisofs, the RISC OS, Apple and Amiga records | done |
| M2 | UDF 1.02–2.60 reading: physical, sparable, virtual and metadata partitions, Finder information | done |
| M3 | UDF writing on one physical partition of rewritable or overwritable media, and formatting | done |
| M4 | USB optical drives: multimedia units and ATAPI over BOT, the disc layout, removal | done |
| M5 | One copy engine keeping holes and attributes: `cp`, `mv`, `fstree`, the file manager, `replace_file` | done |
| M6 | The USB optical QEMU vertical on aarch64, the disc changed mid-run | done |
| M7 | The USB optical vertical on riscv64 and x86_64 | planned |
| M8 | UDF writing on metadata-partition volumes (UDF 2.50 and later) | planned |
| M9 | UDF writing on sparable and write-once (virtual partition) media | planned |
| M10 | Resource forks: UDF named streams and ISO 9660 associated files as `mac.resourcefork` | blocked: ARXFS named streams (`plans/ARXFS-METADATA.md` §4.4) |
| M11 | Software eject and tray locking | planned |
| M12 | MMC media-event notification in place of the two-second removal test | planned |
| M13 | Classic Mac HFS and HFS+ volumes, and hybrid ISO 9660/HFS discs | planned |

## 1. What is read, and from where

- **Recognition is shared.** `lib/fsprobe` decides what an extent holds and,
  for a disc, which tree is mounted: the primary ISO 9660 tree when it
  carries Rock Ridge or RISC OS records, else Joliet, else the ISO 9660:1999
  tree; a bridge disc is UDF unless its ISO 9660 tree keeps what UDF would
  lose. The drivers mount what the probe chose, so the two never disagree.
- **A disc is read from its last session.** The drive reports where that
  session starts and the last block it recorded (`Block::disc_layout`); a
  write-once UDF disc's virtual allocation table is found from the latter.
  A drive that cannot say reads as a pressed disc of one session; one whose
  disc is not ready or changed while it was asked is asked again.
- **A changed disc is a new publication.** The unit is retracted and
  published afresh, under a new binding and a new data window, so nothing of
  the first disc — a held request, a cached block, a volume attached or
  being attached over it — reaches the second.
- **Foreign metadata surfaces as the shared attribute keys**
  (`docs/src/filesystem/metadata-registry.md`): `acorn.*` exactly as ADFS
  presents them, `mac.*` from Apple's records or UDF's Finder information,
  `amiga.*` from Rock Ridge's `AS` entries.
- **ISO 9660 is read-only.** The format records a finished disc.
- **UDF writes only what it can keep consistent.** One physical partition
  with a space bitmap on rewritable or overwritable media, a closed volume,
  a revision up to 2.60; `Udf::write_refusal` names why any other is read
  only. Blocks are taken before anything refers to them and given back on
  failure, so a change that runs out of space leaves the volume as it was.

## 2. The copy

`lib/fscopy` is the one copy (`docs/src/lib/fscopy.md`): a file's bytes with
its holes over `fs_seek_region` (GNU `--sparse` semantics), then its extended
attributes, a refusal classified and gathered into one report per run.
Attributes are carried by default — a divergence from GNU `cp`, which copies
them only under `--preserve=xattr`, because a filetype is part of what a file
is. `--preserve=xattr` makes any refusal fail the copy (the exact-preservation
policy), `--no-preserve=xattr` carries none.

## 3. Remaining work

- **M7.** `tools/qemu`'s `with_optical_disc` already attaches the drive on
  every port; each needs its guest kernel crate and enrolment.
- **M8.** A metadata partition's file entries live in its metadata file, and
  its mirror must be kept identical; writing means allocating within the
  metadata file's own extents and rewriting both copies under one integrity
  open/close.
- **M9.** A sparable partition remaps failed packets through its sparing
  table, which a writer must maintain; a write-once disc appends entries and
  a new virtual allocation table per session.
- **M10.** Waits on ARXFS named streams, which `mac.resourcefork` is stored
  as; the readers then map a UDF named stream and an ISO 9660 associated file
  onto it.
- **M11.** `START STOP UNIT` with LoEj and `PREVENT ALLOW MEDIUM REMOVAL`,
  behind an eject the volume service makes only once the disc's volume is
  detached, so a mounted disc cannot be ejected out from under its readers.
- **M12.** `GET EVENT STATUS NOTIFICATION`'s media class answers a removal or
  an eject request without a command every two seconds.
- **M13.** A driver of its own, `drivers/filesystem/hfs`, whose Finder
  information surfaces through the same `mac.*` keys.
