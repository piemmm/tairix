# tairix-fsprobe

Filesystem signature, label, and identity probe — the one `no_std`
definition of how the head of a block extent is recognised as a supported
filesystem (ARXFS / ext4 / FAT32), which stable 16-byte identity it
carries, and how that identity renders as a short display fingerprint
(`plans/ALIAS.md` §3.8). The volume-manager policy driver
(`drivers/storage/volmgr`) probes with it, and the filesystem drivers
import their shared constants and derivations from it (the ARXFS
metadata-header magic, the ext superblock magic, the FAT32
serial+label+tag identity), so the probe and the mounted driver can never
disagree about what identifies a volume.

## What it provides

- `probe(head)` — recognise a supported filesystem from an extent's
  leading `PROBE_HEAD_LEN` bytes, in a fixed, documented order (ARXFS,
  ext4, FAT32 — most-specific signature first). The bytes are untrusted
  removable-media content: every access is bounds-checked, every field
  is sanity-validated, and no match means `None`, never a guess.
- `ProbedVolume` — the matched filesystem type
  (`tairix_abi::volume::VolumeFsType`), the volume's stable identity
  exactly as the matching filesystem driver publishes it, and its
  recorded label (trimmed; empty where the format records none or holds
  FAT's `NO NAME` placeholder).
- FAT32's authoritative label: `fat32_root_dir_byte` places the root
  directory from the boot sector, and `ProbedVolume::with_fat32_directory_label`
  takes the label entry found in its head (`fat32_directory_label`), which a
  relabel rewrites, over the boot sector's copy.
- `Fat32Bpb` — the validated FAT32 parameter block and the region offsets it
  places, read by both this probe and `drivers/filesystem/fat32`, with the
  FAT directory-entry constants the driver imports.
- `fat32_identity_from_boot` — the FAT32 content-derived identity
  (serial + label + tag; FAT32 has no UUID), shared with
  `drivers/filesystem/fat32`.
- `fingerprint(identity)` — the lowercase Crockford-base32 display
  fingerprint of an identity; callers take the prefix they need
  (catalog collision suffixes, alias identity guards).

## Stability

`experimental`. The probed on-disk formats are external and fixed; the
Rust API may still change while TAIRiX is pre-release.
