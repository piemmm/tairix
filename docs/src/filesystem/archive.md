# Archive driver

An archive file opens as a volume: its members are the volume's files and
directories, read like any other, and written back where the format allows.
An Acorn DFS disc image opens the same way, its catalogues' files served as
an archive's members are.
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
| Acorn DFS disc, `.ssd` or `.dsd` | `dfs` | each side's catalogue of up to 31 files | in place, as DFS writes | load, exec, lock |
| RISC OS Squash file | `squash` | the one file it compressed | compressed again whole, as the Squash module compresses a file | load, exec |

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
  file, a Squash file) is decoded through an index of places the decoder can
  resume at: every bzip2 block and every place a Squash stream's table is
  cleared, at most 4096 of them, and for gzip a checkpoint of the inflater's
  window every 4 MiB at first, at most 128 of them; half are dropped and the
  spacing widened each time they fill.
  A read decodes from the nearest place before it. The opening pass decodes
  the whole stream once, holding each gzip member to its CRC and length, and
  a Squash stream to its header's length, decoded no further than that: its
  cost is the stream's decoded length, which a gzip or bzip2 stream records
  nowhere it can be held to and its format bounds only by its ratio (DEFLATE
  near 1032:1, bzip2 near 10⁶:1), paid in the volume's own driver process
  while the attach waiting on it stays killable.
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
rewrite keeps bzip2's level, gzip's recorded name and time, and a Squash
file's type and date.

A change to a member keeps what its record held that the tree does not: a
zip member's comment, host attributes (switched to Unix where its mode
changes) and extra fields, but those carrying times once its times change; a
tar member's pax records the driver does not read, and its owner's names
while the owner is unchanged.

`fs_set_times` keeps what a member's record holds and no more
(`Format::member_times`): a zip member's modification time to the second in
its extended timestamp, a tar member's both times to the nanosecond in pax
records (the access time recorded from then on), a gzip stream's unsigned
seconds, a Spark or ArcFS member's or a Squash file's typed RISC OS stamp —
a Spark one's MS-DOS stamp where it is untyped. A time the record does not keep is refused
unless the nearest is asked. A member's clock stamps are held the same way, so
what a stat reports is what the committed archive reads back.

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
`FileSwitch` byte, which has no place for public execute or private. Spark
and ArcFS names cross through the one RISC OS name conversion
(`acorn::leaf_from_riscos`, `acorn::leaf_to_riscos`), as ADFS's do: RISC OS's
Latin-1 and UTF-8, a dot and a slash trading places as a space and a hard space
do, and no name RISC OS could not have written in either direction. A member named as no RISC OS name can be
is left out, and the archive is not written back, which would lose it.

## DFS disc images

A DFS disc image is a dump of a BBC Micro disc's sectors: an `.ssd` holds one
side, a `.dsd` both, alternating track by track. The image's name ending
says which, and an image named neither way is taken as double-sided only
where it runs past its first side's sectors. An `.ssd`'s files are the
volume's root; a `.dsd` serves its sides as the folders `Drive 0` and
`Drive 2`, as DFS numbers them, a side whose catalogue is not a DFS
catalogue left out.

A file is served by its full DFS name: its directory, a dot, then its name —
`$.ELITE`, `B.DATA` — a `/` in the name served as `.`, as every Acorn name
crosses. A name compares as DFS compares it, letters in either case. A new
file is named the same way, its name one to seven characters none of which
DFS reads a meaning into (`.`, `:`, `"`, `#`, `*`, a space); a side holds
only files, at most 31 of them, and no folder is made in one. A name in the
catalogue no served name can spell — one holding a `.` — is left out, and the
disc is not written back. A file's load and execution addresses are its
`acorn.loadaddr` and `acorn.execaddr`, the I/O processor's given as
`&FFFFxxxx`, and its lock is the `L` of its `acorn.attr`; every file can be
read and written, so an attribute without `R` and `W` is refused, as is an
address no eighteen-bit field can hold. An address that reads as a typed
object's is still an address: no write stamps a time into it. DFS records no
times.

A commit changes only what changed, as DFS itself would. A file whose bytes
are unchanged keeps its sectors; a file written goes back where it was if it
still fits there, else into the first gap from the catalogue up that holds
it, and only where no gap does is the side compacted, every file laid end to
end. Every other sector — free space, and whatever a loader keeps outside the
catalogue — is copied as it was, a name keeps the top bits its catalogue held
until it is renamed, and a catalogue holding the same entries is left as it
was, its cycle number unstepped. An image grows only to the end of the last
sector written; a short image's missing tail reads as zeros, as the sector
dump's format means. A write past a side's sectors or its 31 entries is
refused for want of space.

A disc formatted by Watford DFS for 62 files keeps a second catalogue in the
sectors DFS 1.20 counts free; one is served read only, its first catalogue's
files alone. The Open Micro emulator locks a disc it may write
([Open Micro](../desktop/openmicro.md)), so such a disc is not mounted while
it is in one of the emulator's drives, and a disc already mounted goes into
a drive write-protected.

## Squash files

A Squash file is one RISC OS file compressed by the Squash module, its
header (`SQSH`) recording the file's length and its load and execution
addresses; RISC OS types it `&FCA`. Its one file is named as the Squash
file is, less any `,fca` ending, and typed and dated by the addresses. A
stream that decodes to other than the header's length is not the file the
header describes, and the Squash file is refused. A rewrite compresses the
file whole, as the Squash module compresses a file given it whole with
room for all it makes, byte for byte the stream RISC OS writes, its codes
those of the one compress-style LZW coder `lib/compress` holds. The header
has no place for attributes, so the file's are those it is read with and
no other is taken, nor a name, a second file, or more than 4 GiB less a
byte.

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
their own harnesses (`lib/compress`: `fuzz_bzip2`, `fuzz_arc`,
`fuzz_squash`). The Squash stream is pinned against streams and traces
RISC OS 5.30's Squash module was recorded writing. The least archive of each
format, the streams members are packed in, and an archive written back by
the driver are `tests/integration/archive_image`'s — a DFS disc of each
layout and a Squash file among them — shared by the unit tests, the fuzz
harness and the disk-image vertical, which mounts an archive of each format,
a DFS disc and a Squash file in QEMU with `mount`, reads each with its RISC
OS fields, writes a file in every one but the RAR — the Squash file's one
file rewritten, a second refused — and gives it a filetype or a load
address, ejects it, and reads the file and its field back once it is
mounted again.
