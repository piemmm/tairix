# Archive driver

An archive file opens as a volume: its members are the volume's files and
directories, read like any other, and written back where the format allows.
The driver is `drivers/filesystem/archive` (`tairix-drv-fs-archive`), one
served-filesystem process per attached archive
([Volumes served from user space](./served.md)); the attach it is served
through is the disk-image attach ([Disk images](./images.md)). The staged
design is `plans/ARCHIVES.md`.

## Formats

| Format | `mount -t` | Read | Written | RISC OS fields |
|---|---|---|---|---|
| zip | `zip` | stored, deflate, bzip2; ZIP64; UTF-8, code page 437 and Info-ZIP Unicode names; Unix modes and links; extended times | deflate or stored | the `SparkFS` field |
| tar | `tar` | v7, ustar, pax and GNU headers: long names and links, times to the nanosecond, owners, hard links | ustar, a pax record where a field needs one | `SCHILY.xattr.acorn.*` pax records |
| tar in gzip or bzip2 | `tar.gz`, `tar.bz2` | multi-member gzip, concatenated bzip2 | compressed again whole | as tar |
| one gzip or bzip2 stream | `gzip`, `bzip2` | the one file it holds | yes | none |
| Spark, and plain ARC | `spark` | every ARC and Spark method: stored, packed, squeezed, crunched, squashed, compressed; nested directories; ARC 6's methods skipped, leaving the archive read only | stored or compressed | load, exec, attributes |
| ArcFS | `arcfs` | stored, packed, crunched, compressed | stored or compressed | load, exec, attributes |
| RAR 1.5 to 7 | `rar` | every RAR compression: 1.5's, 2's LZ and audio, 3's LZ, PPMd and standard filters, 5's and 7's; solid archives | never | none |

The kernel's probe (`lib/fsprobe`) recognises each format by its content,
never its name, and the driver recognises it again with the same readers, so
the two never disagree. An archive must begin with its signature: a
self-extracting program, or another file with a RAR appended, is not opened.
A zip is found from its end, so a self-extractor's program ahead of it is
kept as it is.

## Reading

Every count, length, offset and name an archive states is checked before
use. A member whose path climbs out of the volume, collides with another, is
too deep, is a kind no volume holds (a device, a pipe, a sparse tar member, a
tar volume label), names itself in a character set it breaks, or whose data
lies past the archive's end is left out, and an archive missing a member is
never written back, since writing would lose it. An archive is refused whole
only where nothing in it can be believed.

- **zip** — the central directory is read by its own length, so no member is
  lost to a count the end record gets wrong; a count no 16-bit wrap explains
  leaves the archive unwritten. An extended timestamp is signed, as its
  definition says, but read unsigned where the MS-DOS stamp beside it puts it
  past 2038.
- **tar** — a global pax header holding only comments, as `git archive`
  writes, is kept ahead of every member; one saying anything of the members
  is read as saying it, and leaves the archive unwritten, as does anything
  past the end marker but zeros. `./` is the root's own record. A path two
  members name serves the later, as extracting the archive leaves it, and the
  archive is not written back.

A member is checked against what its archive records — CRC-32, CRC-16, or
RAR 5's BLAKE2sp — as a read passes through it in order, whether its bytes
are stored or compressed; a member failing its check is refused, never
served. A stored member read out of order is checked only by a later read
from its start.

- **Members compressed on their own** (zip, Spark, ArcFS, a non-solid RAR
  member) are decoded by a cursor that keeps its place, so a member read in
  order is decoded once; a read before the cursor starts a fresh one. A few
  cursors are kept, one per reader working through a member.
- **An archive that is one stream** (a compressed tar, a lone gzip or bzip2
  file) is decoded through an index of places the decoder can resume at:
  every bzip2 block, and for gzip a checkpoint of the inflater's window
  every 4 MiB at first, at most 128 of them, half dropped and the spacing
  widened each time they fill.
  A read decodes from the nearest place before it. The opening pass decodes
  the whole stream once, holding each gzip member to its CRC and length: its
  cost is the stream's decoded length, which the format bounds (DEFLATE near
  1032:1, bzip2 near 10⁶:1) and no bound short of that tells from a file of
  zeros, paid in the volume's own driver process while the attach waiting
  on it stays killable.
- **A bzip2 member** is decoded in a workspace no larger than its length
  needs, five quarters of it, rather than its level's block.
- **A solid RAR archive** codes its members as one stream, each carrying on
  from the decoder's state where the last left it, so a member there is read
  by decoding its run from its first member that is not solid. A cursor
  keeps a run where the last read stopped, so a reader working through the
  archive in order decodes it once.

RAR's decoders run in fixed memory a stream cannot inflate: a window grows
with what is decoded into it, up to the dictionary — no further than the
coding's distances reach, whatever a header claims — or the run's length,
whichever is smaller; a read decodes no further ahead than it asked, but for
a filter's block; PPMd lays its model on a heap of the size its block
states, at most 256 MiB, the format's limit, asked of the allocator zeroed
and written only as the model's symbols build it, so where pages are backed
on first touch a stream holds what it decoded rather than what it declared,
and a stream declaring model after model has each laid on the last one's
heap, never another zeroed; at most four runs are decoded at once, and one
needing memory another holds, as it starts or partway through a read, takes
it. RAR 3's filters are run when their
program is one of the six standard ones (x86, x86 with jumps, Itanium,
delta, RGB, audio), recognised by its checksum and length, one defined over
the last one's block running on what that one gives; no virtual
machine is run, and a member needing any other program is refused. A member
named as no volume can serve keeps its place in its solid run, unlisted; one
whose header cannot be read refuses every member carrying on from it.

## Writing

The archive is never written in place. A change waits in the attachment's
staging file, and a commit writes the next archive into its other work file
— the bytes of a member nothing has changed copied there by the kernel —
which the kernel then makes the archive, so a crash leaves the old archive
or the new one whole. A compressed archive is compressed again whole; a
rewrite keeps bzip2's level and gzip's recorded name and time.

A change to a member keeps what its record held that the tree does not: a
zip member's comment, host attributes (switched to Unix where its mode
changes) and extra fields, but those carrying times once its times change; a
tar member's pax records the driver does not read, and its owner's names
while the owner is unchanged.

A member this driver cannot read — an encrypted zip member, one coded by a
method it does not decode — is kept as it is through every commit, under its
own record's method, flags, stamp and version needed, its name flagged UTF-8;
a change that would rewrite its record (a rename, an attribute change) is
refused with the member's own reason.

A change stands once it is made. A commit that fails stops the archive being
written, and every flush after answers its reason. A member removed while
held is moved to the staging file first; one whose bytes cannot be read —
refused, or found damaged as they are moved — answers every read refused
from then on, and never stops a commit.

A format keeps what it has a place for: modes in zip and tar, owners in tar,
RISC OS fields wherever the format records them, the root's own mode and
owner in a tar, as `./`, and nowhere else, so a change to it there is
refused. A name a format cannot spell is refused: ArcFS keeps ten
characters, Spark twelve, both in RISC OS's Latin-1. RAR is read only and
refuses every change.

## RISC OS fields

A member's load and exec addresses and attributes are its `acorn.*`
attributes (`lib/fsmeta`'s acorn preset). A typed object's stamp is its
modification time: setting the stamp sets the time, a change to its bytes
stamps it, and an object typed afresh is dated as the member is. A member made
here, or carried from a format that records none, is written typed as Data,
and a directory as the format's own type. The attribute bits a format holds
are the bits it takes: a tar's records spell every one, the others' a
`FileSwitch` byte, which has no place for public execute or private. Spark and ArcFS names cross between RISC OS's Latin-1 and
UTF-8, a dot and a slash trading places as ADFS trades them.

## Refused archives

- **Encrypted headers** (RAR) — `PermissionDenied`; an encrypted member is
  listed and refused when read, and in a solid RAR so is every member after
  it.
- **One volume of several** (zip, RAR) — `Unsupported`.
- **A RAR member of unknown length** (written from a pipe) — refused when
  read.

## Tests

Each format's reading and writing is tested over archives built in the
tests, with the formats' foreign tools' output pinned where one exists. RAR
is pinned against archives the RAR encoder wrote (rarfile's test archives,
ISC), streams for the filters and PPMd synthesised and checked against
libarchive, and RAR 5 streams an encoder in the tests writes across every
symbol, filter and solid run. RAR 7's algorithm is decoded as RARLAB
documents it, but no archive written with it has been found to test against.
`fuzz_archive_open` mutates a base of every
format, RAR included, through open, walk, read and commit; the codecs carry
their own harnesses (`lib/compress`: `fuzz_bzip2`, `fuzz_arc`). The least
archive of each format, the streams members are packed in, and an archive
written back by the driver are `tests/integration/archive_image`'s, shared by
the unit tests, the fuzz harness and the disk-image vertical, which mounts an
archive of each format in QEMU with `mount`, reads it with its RISC OS
filetypes, writes and types a file in every one but the RAR, ejects it, and
reads the file and its type back once it is mounted again.
