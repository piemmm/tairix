# `tairix-fscopy`

The one file copy every copying tool runs: `cp`, `mv` across volumes,
`fstree`, the file manager's paste, and the runtime's `replace_file`. A copy
moves a file's bytes with its holes, carries its extended attributes as far as
the destination can keep them, reports once what it could not, and never
copies a directory into itself. The
engine reaches files only through three seams — `Source`, `Target` and
`AttributeStore` — so it is host-tested against fixtures. A tool that reaches
files by path offers them as `PathFiles` (read, size, region, write, length,
each by path), and `PathFile::new(files, path)` is one of them as a `Source`
and a `Target`, so no tool carries an adapter of its own. `tairix_rt::copy`
implements the seams over the running system (`RtFile`, `RtAttributes`,
`RtAncestry`, and `OpenFiles`, the per-path handle cache a path-based tool keeps between
chunks, which is its `PathFiles`).

## Bytes and holes: `DataCopy`

`DataCopy::new(len, sparse)` copies a file `len` bytes long a chunk at a time
(`step`, or `run` to the end), each chunk at most the buffer the caller lends
it — `CHUNK_LEN`, one `fs_read`'s worth, for a tool that copies large files.
Where the source stores no data (`fs_seek_region`), nothing is read or
written; the target's length is set last, and only when its writes stopped
short of it. `Sparse` follows GNU `cp --sparse`:

| `Sparse` | Holes | Stored zeros |
|---|---|---|
| `Auto` (default) | kept | written |
| `Always` | kept | a file-aligned `ZERO_UNIT` (4096-byte) unit of them left unwritten |
| `Never` | written as zeros | written |

A source with no word on its holes (`NotSupported`) is copied as all data. A
source that shrinks while it is copied ends the copy where it ended: a read
finding the end stops it there, and a copy whose data runs out asks the
source's length (`Source::size`) before taking the rest for a hole, so a
shrunken file is never padded back out to its old length. A run a punch
emptied between the copy's two questions is asked for again further on.
Regions that contradict themselves (`DeviceFault`: data answered before where
it was asked for, or the same run found empty twice), a read claiming more
bytes than its buffer held (`LengthOutOfRange`) and an empty buffer
(`BufferTooSmall`) are refused. A failure names its side, `DataError::Read` or
`DataError::Write`.

## Attributes: `copy_attributes`

`copy_attributes(store, source, destination, mode)` sets every attribute the
source lists on the destination. A key the source no longer carries
(`NoData`) is passed over; a source whose volume keeps none (`NotSupported`)
has none to carry; any other read failure fails the copy. What the
destination refuses is classified as a `Refusal`:

| Refusal | Errno |
|---|---|
| `Unsupported` | `NotSupported`, `ReadOnly` |
| `NotRepresentable` | `OutOfRange` |
| `NoRoom` | `NoSpace`, `LengthOutOfRange` |
| `NotPermitted` | `PermissionDenied` |

`Attributes::Carry` (the default) leaves those keys and answers each with its
refusal; `Attributes::Require` (`cp --preserve=xattr`) fails the copy on the
first; `Attributes::Skip` (`cp --no-preserve=xattr`) carries nothing. A
refusal of any other kind fails every mode.

## The report: `Omissions`

A tool notes what each node's copy left (`note`), gathers runs together
(`merge`, for a batch), and reports once at the end: `report(producer)`
answers a `Report` holding

- `summary`: one clause the tool frames for its own surface — `cp: warning:
  …` on `stderr`, `fstree`'s message line, the file manager's warning alert —
  naming how many files lost which keys (at most eight named, then "and
  others") and why, each key in its display form
  (`tairix_fsmeta::attr::display_value`), so control bytes a volume's own key
  carries never reach a terminal as an escape sequence;
- `record`: one framed `stdinfo` `omission` record, code
  `fs.attributes_not_kept`, whose `ai` object lists the refusal codes, the
  file count, the keys named and whether that list is exhaustive.

## No copy of itself: `landing` and `Descent`

A tree copy whose destination lies inside its source would copy each new copy
again without end. `landing(fs, source, source_id, target)` answers before
anything is written whether `target` is `Apart` from the directory `source`,
the `Same` directory, or `Within` it. It reads the filesystem through the
`Ancestry` seam — a path as the kernel resolves it (`fs_realpath`, the last
component allowed absent) and a node's identity — and compares the two
resolved paths, then the identity of each directory on the target's resolved
path with the source's, so neither a link, a `..` nor a second spelling of a
volume hides the source. A volume that offers no identity (`FileId::NONE`) is
judged by the paths alone.

`Descent` keeps the copy honest while it runs: `enter(source, made)` records
the source directory being copied and the directory made for it, `refuses(id)`
tells the walk a directory it is already inside, and `leave()` pops the level.
A directory moved into the source meanwhile, or a link leading back up the
tree, is therefore refused where the walk meets it. It holds two identities a
level, so it costs the walk's depth, never the tree's size. `cp`, `mv`,
`fstree` and the file manager's paste refuse through both, and
`tairix_rt::copy::RtAncestry` is the seam over the running system.

## One run, one destination: `Created`

`mv a/x b/x d/` would put `b/x` over the `a/x` it had just moved there, whose
source is already gone. `Created` records the nodes a run's operands put in
place — by identity, so another spelling of the name or a case-folding volume
does not hide one, and by path on a backing that offers no identity — and
`cp` and `mv` refuse a later non-directory operand whose destination it holds,
in GNU's words ("will not overwrite just-created 'd/x' with 'b/x'"). A
directory still merges into, or replaces the empty, directory an earlier
operand made. The record is two fallible hash sets under the process's keyed
hasher, since a name can be a hostile volume's to choose; a heap that cannot
grow it stops the run rather than let an operand through unguarded.

## Test support

The `test-support` feature carries `fixture::resolve`, the kernel's path
resolution in miniature — each link on the way followed, each `..` applied,
a cycle cut off as `LinkLoop` — which the copying tools' in-memory test
filesystems answer the containment check with, so none carries its own copy.

The crate is `no_std` + `alloc`, forbids `unsafe`, and depends on `lib/abi`,
`lib/fsmeta`, `lib/path`, `lib/collections` and `lib/hash`.
