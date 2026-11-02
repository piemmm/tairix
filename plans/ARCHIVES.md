# ARCHIVES.md — Archives as volumes

Binding under `AGENTS.md`. A user double-clicks a zip, a tar (plain, gzip'd or
bzip2'd), a RISC OS Spark or ArcFS archive, or a RAR archive, and its contents
appear as a volume at `/Storage/<name>`, private to them, which every program
reads and — for every format but RAR — writes: copy in and out, rename, move,
delete, make directories. RISC OS filetypes, load and exec addresses and
access bits travel with each member as the `acorn.*` attributes, so a RISC OS
file keeps its type through an archive and back. It consumes and never
contradicts `plans/DISK-IMAGES.md` (the attach, presentation, lifetime and
eject an archive shares with a disk image), `plans/FSSERVE.md` (the served
volume), `plans/ARXFS-METADATA.md` §6.3 (attributes through archives) and
`plans/FILES-INTERACTION.md` (activation).

## Ledger

| id | What it is | Status |
|---|---|---|
| AR1 | Recognition in `lib/fsprobe`: zip, tar, gzip, bzip2, Spark, ArcFS and RAR by their signatures alone, never by decompressing | done |
| AR2 | The kernel archive attachment: an image attachment of an archive's file and directory, its exact byte length, its work files, the copy between them, and the commit that makes a work file the archive atomically | done |
| AR3 | The served archive volume, one driver crate for every format: the member tree, node identities and pins, attributes, staged writes, and the commit that writes the next archive into a work file | done |
| AR4 | `lib/compress`: the inflater's checkpoints and bzip2 decode and encode; the driver's index of a compressed stream's resumable places | done |
| AR5 | tar: v7, ustar, pax and GNU headers, plain or in gzip or bzip2, read and written; a lone gzip or bzip2 stream as a volume of its one file | done |
| AR6 | zip: stored, deflated and bzip2 members, ZIP64, the SparkFS `0x4341` RISC OS field, UTF-8 names, extended times; read and written | done |
| AR7 | `lib/compress::arc` and Spark: the ARC methods (stored, packed, squeezed, crunched, squashed, compressed) and Spark's RISC OS headers and nested directories; read and written | done |
| AR8 | ArcFS archives over the same ARC methods; read and written | done |
| AR9 | RAR 1.5–4 and RAR 5 headers, the unpack15/20/29/50 decoders, PPMd variant H and the standard filters; read only | done |
| AR10 | The desktop and the command line: archive content types and icons, activation mounting an archive, *Eject*, `mount ARCHIVE` | done |
| AR11 | A QEMU vertical: an archive of each format mounted, read, written where writable, ejected, and read back with its RISC OS attributes | done |

## 1. What an archive is

- **One file, one volume.** The archive's members are the volume's tree; a
  member path's components are its directories, made where the archive names
  none; an empty component and `.` name nothing and are passed over. A lone
  gzip or bzip2 stream that holds no tar is a volume of one file, named by
  the gzip header's name or the archive's own name less its ending.
- **Recognised by content.** The kernel's probe (`lib/fsprobe`, AR1) knows each
  format by its signature: zip by its end-of-central-directory record, tar by a
  header whose checksum holds, gzip and bzip2 by their stream magic, Spark by
  its marker and method bytes, ArcFS by `Archive\0`, RAR by `Rar!\x1a\x07`. It
  never decompresses: whether a gzip or bzip2 stream holds a tar is the
  driver's question, answered in its own sandboxed process. An archive must
  begin with its signature, so a self-extracting program or a RAR appended
  to another file is not an archive; a zip is found from its end, so a
  self-extractor's program ahead of one is kept as it is.
- **Hostile input.** An archive is a stranger's bytes, parsed only by its
  driver, a capability-empty served-volume process (`plans/FSSERVE.md`) holding
  nothing but its own attachment. Every count, length, offset and name is
  validated before use. A member that cannot be believed or served is left
  out, and an archive missing a member is never written back, since writing
  would lose it (`WriteRefusal::UnsupportedFeature`); an archive is refused
  whole only where nothing in it can be believed.
- **Names.** A member's name is cut into components at `/` (and, for Spark and
  ArcFS, read as RISC OS spells it, `.` and `/` exchanged as ADFS exchanges
  them); a component that is empty, `.`, `..`, or not a name the path grammar
  admits is left out with the member, so no member names anything outside the
  volume. A name in zip's CP437 is converted; a zip name flagged UTF-8 is held
  to UTF-8.
- **Bounds** (fixed containment bounds, §24.4): `MAX_MEMBERS` members
  (1 << 20), `MAX_NAME_BYTES` per path, `MAX_DEPTH` directories, the index's
  bytes charged to the driver's own memory limit; an archive past one is
  refused with that reason. A member's size is 64-bit throughout.

## 2. The archive attachment (AR2)

- **The same attach.** An archive is attached through `image_attach`
  (`plans/DISK-IMAGES.md` §2): the same descriptor authority, presentation,
  per-principal identity, bound, lock, eject and session lifetime. What differs:
  - *Exact length.* The volume's description carries the file's byte length;
    the trailing partial block reads as the file's bytes then zeros, so a
    driver finds a record at the very end.
  - *Its directory.* A writable archive is attached with a descriptor of its
    directory and its name beside the file's descriptor. The kernel confirms
    the name names that file and that the caller may write the directory, and
    holds the directory as the attacher's identity frozen at the attach, as it
    holds the file. A directory reached through a read-only view attaches the
    archive read only: the commit holds the directory through its volume's own
    projection, which the view narrows.
- **Never written in place.** A writable archive's driver never writes the
  archive. It has up to `ARCHIVE_WORK_FILES` (2) **work files**: growable,
  anonymous files on the archive's own volume, charged to the attacher, read,
  written and truncated through `volume_read`/`volume_write` by region, and
  gone at detach. A work file is only ever cut, and written from no further
  than its end, so no request has the volume fill zeros. `volume_ctl` copies a
  range between regions in the kernel, so a member kept unchanged is never
  carried through the driver. Uncommitted work files are let go, the next
  archive taken back out of the directory, as the volume is detached or left
  unserved, or as a driver serves it in place of one lost — the loss itself
  cannot park to do it; one whose file's volume is gone goes with it.
- **The commit.** `volume_ctl(Commit { work })` makes a work file the archive:
  the kernel makes it durable, gives it the archive's owner, mode, ACL and
  attributes (its `acorn.filetype` among them), then renames it over the
  archive's name in the held directory — only where that name still names the
  attached file, else the commit fails closed and the archive is unchanged. A
  replace that answers an error is settled by what the name then names: the
  next archive (committed), the archive (unchanged), or nothing — a format
  replacing by unlinking first stopped between — when the next archive is
  moved into the name, or else kept under its own name rather than lost. A
  record the archive took during the commit is given to the new one too. The
  attachment then reads the new file as region 0, its old lock let go once the
  directory is durable; a directory that cannot be made so is the next flush's
  answer (`fs.image.archive_commit`, 4326, records each such shortfall). A
  crash leaves the old archive whole.

## 3. The served archive (AR3)

One driver crate, `drivers/filesystem/archive`, serves every format: the
member tree, cursors, staging and commit are the formats' common core, and
no second consumer exists to put them in `lib/*`.

- **The tree.** One node per member and per directory, ids stable for the
  attachment, each pinned as `plans/FSSERVE.md` requires; a member's data is a
  source: a range of the archive (and how it is compressed), or runs of the
  staging work file once written.
- **Reading.** A stored member reads straight from the archive; a compressed
  one through its format's decoder, from the nearest point it can resume at: a
  member's start, a bzip2 block, a gzip index point (AR4), or for a solid RAR
  member its run's first member. A cursor keeps a decoder where the last read
  left it, a few at once. Each member is held to its check — CRC-32, CRC-16,
  BLAKE2sp — as a read passes through it in order, stored or compressed, and
  refused on a mismatch.
- **Attributes.** Each format's RISC OS fields are answered as `acorn.*`
  (`lib/fsmeta`'s acorn preset): Spark and ArcFS from their headers, zip from
  the `0x4341` field, tar from pax `SCHILY.xattr.acorn.*` records; set ones are
  written back the same way. A tar member named `name,xyz` keeps its name: the
  desktop already reads such an ending as a filetype.
- **Writing.** A write, create, rename, remove or attribute change edits the
  tree and stages new data in the staging work file, a write-back held to the
  commit window (`lib/writeback`'s policy) and to `sync`, close and eject. The
  commit writes the next archive into the other work file — members kept
  unchanged copied by the kernel, staged ones encoded — then commits it (§2).
  gzip and bzip2 tars are re-encoded whole, as their streams require, keeping
  bzip2's level and gzip's name and time. A changed member keeps what its
  record held that the tree does not (`Node::notes`): a zip member's comment,
  host attributes and extra fields, but those carrying times once its times
  change; a tar member's unread pax records, and its owner's names while the
  owner is unchanged. A member the driver cannot read (an encrypted zip
  member, an undecoded method) is kept as it is under its own record's method,
  flags, stamp and version needed, and a change that would rewrite its record
  is refused with its reason. A change stands once made; a commit that fails
  stops the archive being written (`WriteRefusal::CommitFailed`) and is every
  later flush's answer. A held member removed, whose bytes cannot be read,
  never stops a commit.
- **RAR** is read only: its compression is proprietary to its authors, and no
  RAR is written (`WriteRefusal::FormatReadOnly`).

## 4. Formats

- **tar** (AR5): v7, ustar, pax (`path`, `linkpath`, `size`, `mtime` to the
  nanosecond, `uid`/`gid`, `uname`/`gname`, `SCHILY.xattr.*`) and GNU (`L`,
  `K`). Written as ustar with pax records only where a field needs one.
  Symbolic links read as links, hard links as their target's bytes; devices,
  pipes, sparse members (GNU's pax sparse members too) and volume labels have
  no volume node and are left out. A global header holding only comments is
  kept, ahead of every member; any other is read and leaves the archive
  unwritten, as do two members of one path (the later served, as extraction
  leaves it) and anything but zeros past the end marker. `./` is the root's
  record, which a change to the root writes from then on.
- **zip** (AR6): local and central records, data descriptors, ZIP64, methods
  stored (0), deflate (8) and bzip2 (12) read, deflate or stored written; an
  encrypted or otherwise-compressed member is listed and refused on read with
  its reason. Unix modes, links and the extended-time field read and written,
  that field signed but unsigned where the MS-DOS stamp puts it past 2038; the
  central directory read by its own length, a count disagreeing that no
  16-bit wrap explains leaving the archive unwritten; split archives refused.
- **Spark** (AR7): ARC headers with Spark's RISC OS load, exec and attributes;
  a directory is a nested archive. Methods stored, packed, squeezed, crunched,
  squashed and compressed read; stored and compressed written. A plain PC ARC
  reads as Spark with CP437 names; ARC 6's methods are skipped and leave the
  archive read only.
- **ArcFS** (AR8): the header table and data, over the same methods; the
  table is written afresh, sorted as RISC OS sorts.
- **RAR** (AR9): RAR 1.5–4 and RAR 5 archives, solid and not, in the driver's
  `rar` module: 1.5's adaptive coding, 2's LZ and audio modes, 3's LZ, PPMd
  variant H and its six standard filters (recognised by bytecode checksum and
  length; no virtual machine is run, and any other program is refused), and
  RAR 5's, with RAR 7's wider distances; a RAR 3 filter over the last one's
  block runs on its output. Members are held to CRC-32 or BLAKE2sp, a stored
  member's check never taken by another reader's seek. A decoder's window
  grows with what is decoded, up to the dictionary (no further than the
  coding's distances reach) or the run's length, and a read decodes no
  further ahead than it asked but for a filter's block; a member no volume can
  name keeps its place in its solid run, and one whose header cannot be read
  refuses every member carrying on from it; PPMd's heap is asked of the allocator
  zeroed and written only as its model builds, so a stream holds what it
  decoded rather than the size it declared; at most four runs are decoded
  at once, and a run short of memory takes what the others hold.
  Encrypted headers, encrypted members (and the solid members after them),
  one volume of several, and a member of unknown length are refused with
  their reason. RAR 7's algorithm has no archive yet found to test against.

## 5. The desktop and the command line (AR10)

- `lib/browse` types zip, tar, tar.gz/tgz, tar.bz2/tbz2, gz, bz2, spk, arc and
  rar, and the RISC OS filetypes &A91 (zip), &DDC (Spark), &3FB (ArcFS), &C46
  (tar) and &F89 (gzip); activating one mounts it, exactly as a disk image
  (`Activation::MountImage`), read-write where the user may write the file and
  its directory. Their icons are the archive masters (`plans/ICONS.md`).
- `mount ARCHIVE [TARGET]` mounts an archive as it does an image, and
  `unmount` ejects it.

## 6. Tests (AR11)

Every decoder has a fuzz harness (§19.6) and unit tests over hand-built
archives; each format round-trips a tree with RISC OS attributes; a commit's
failure leaves the archive whole. The least archive of each format, the
streams members are packed in and an archive written back by the driver are
`tests/integration/archive_image`'s, which the driver's tests, its fuzz
harness and the disk-image vertical share. That vertical
(`tairix-test-images-qemu-aarch64`, `plans/DISK-IMAGES.md` DI7) copies a zip,
a tar, a gzip'd and a bzip2'd tar, a Spark and an ArcFS archive, each holding
`ReadMe` typed as RISC OS text, into a home; mounts each with `mount`, reads
`ReadMe` and its filetype, writes and types a file, ejects it — the commit —
and mounts it again to read both files and their types back. A RAR archive
the RAR encoder wrote is mounted beside them, its members read whole through
their BLAKE2sp checks, and a write to it refused.
