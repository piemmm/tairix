# tairix-fsprobe

Filesystem signature, label, and identity recognition — the one `no_std`
definition of what a block extent holds (a RAID member, ARXFS, ext4, FAT32,
ISO 9660, UDF or ADFS), which stable 16-byte identity it carries, and how that
identity renders as a short display fingerprint (`plans/ALIAS.md` §3.8). The
volume manager (`drivers/storage/volmgr`) and the kernel's volume service
recognise with it, and the filesystem drivers import their shared structures
from it, so the probe and the mounted driver can never disagree about what
identifies a volume or which tree of a disc is mounted.

## What it provides

- `recognise(dev)` — what a `ByteDevice` holds, in a fixed, documented order:
  a RAID array member, then the formats whose signature opens the extent
  (ARXFS, ext4, FAT32 — most specific first), then the disc formats read from
  the disc's last session (ISO 9660 and UDF, a bridge disc recognised as UDF
  unless its ISO 9660 tree carries Rock Ridge or RISC OS records), then ADFS,
  whose oldest layout carries the weakest signature. Every byte is untrusted:
  every access is bounds-checked, every field validated, and no match is
  `None`, never a guess.
- `ProbedVolume` — the matched `VolumeFsType`, the volume's identity exactly as
  its driver publishes it, its label (trimmed; empty where the format records
  none or holds a "no label" placeholder), and the span it covers.
- `iso9660` — descriptor-set reading (ECMA-119 and High Sierra), the choice of
  tree (primary with Rock Ridge or RISC OS records, Joliet, the ISO 9660:1999
  enhanced tree), and the directory record the ISO 9660 driver walks.
- `udf` — the volume recognition sequence, the anchor and the block size it
  fixes, descriptor tags and their CRC, the prevailing volume descriptors,
  OSTA Compressed Unicode, and the allocation descriptors the UDF driver reads.
- `adfs` — the boot block, disc record and old map ADFS volumes are recognised
  by, shared with `drivers/filesystem/adfs`.
- `Fat32Bpb`, `fat32_identity_from_boot` and the FAT32 directory-label
  helpers, shared with `drivers/filesystem/fat32`.
- `fingerprint(identity)` — the lowercase Crockford-base32 display
  fingerprint of an identity; callers take the prefix they need.
- `name` — the catalog-naming policy a volume is published in `/Storage`
  under: its label in kebab case, else its transport and fingerprint, a name
  in use or reserved taking ever more of its own fingerprint. The volume
  manager names a recognised volume by it and the kernel a network volume.

## Stability

`experimental`. The probed on-disk formats are external and fixed; the
Rust API may still change while TAIRiX is pre-release.
