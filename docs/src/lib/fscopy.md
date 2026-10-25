# `tairix-fscopy`

The one file copy every copying tool runs: `cp`, `mv` across volumes,
`fstree`, the file manager's paste, and the runtime's `replace_file`. A copy
moves a file's bytes with its holes, carries its extended attributes as far as
the destination can keep them, gives what it makes the mode `cp` gives it,
reports once what it could not carry, and never copies a directory into
itself. The engine reaches files only through held nodes — the tree seam's
`Node` and `Dir`, and the `Source` and `Target` data seams a held file is read
and written through — so it is host-tested against the in-memory fixture.
`tairix_rt::copy` implements the seams over the running system: a `File` is a
held node, a `Dir` a held directory, and `WorkingDirectory` the `Roots` a
tool's operands are opened from.

## The tree seam: `tree`

A tool walks a tree through directories it holds open, reaching each child by
its name from there and never through a link it was not asked to follow, so a
name swapped for a link between a tool's reading it and its acting on it leads
nowhere new. `Node` is a held node (its stat, mode, owner, attributes and
sync); `Dir` a held directory, whose children are listed whole (`entries`,
each `Entry` its name, kind and the identity the listing gave it), opened
(`open_dir`, `open_file` for reading or for writing alone, `open_node`, each
never through a link; `enter` and `follow` through a final link, where an
operand's own path leads), made (`create_file`, `make_dir`, `symlink`, `link`
with a final link followed or not), read (`readlink`) and removed or renamed
(`unlink`, `rename`, each able to name the node it expects through
`NodeExpect`). `Roots` opens a tool's operands from the working directory: a
directory with its final link followed or not (`open_dir`), the directory
holding an operand's last component (`parent`), or the node itself
(`open_node`). `open_operand` opens an operand through its final link as the
directory or the other node it names, telling a dangling link from a refusal.
`entry_name` is an operand's last component as a name its parent holds,
never a root (`names_a_root`), `.` or `..`.

`Cursor` walks a held directory depth first and keeps its place between
calls, so a paused or time-sliced operation holds one between its units:
`reach` answers the next entry (`Reached::Entry`) or the directory just left
(`Reached::Left`, handed the directory held), each directory's entries read
whole before any is reached; `descend` goes into the entry it is at, opened
through no link and refused (`NotADirectory`) unless it is a directory now
whatever its listing said, and `descend_into` into a directory the caller
opened from it, so a walk goes on beneath the very node acted on. A directory
the cursor is already inside is refused (`LinkLoop`), so a volume whose tree
loops cannot hold a walk in it. One path buffer serves every level, so a
cursor costs the tree's depth and its listings, never its size; what the
heap cannot hold is refused as `OutOfMemory`. `walk(root, root_path,
visitor)` is the loop over one: a `Visitor` answers each entry `Step::Skip`,
`Descend` or `Into(held)`, every descent ending in one `leave` or one
`refused`, and the root's own listing failing is `unlisted_root`.

`each_node` is the walk of a tool that changes a tree node by node through
each node's own descriptor, a directory before its entries (`chmod`, `getcap`,
`setcap`): an operand through its final link, every link met inside left
alone, and a directory changed through a node handle — which needs no read
access — then gone into only while its name still holds that node. An
`EachNode` tool answers each node, link, dangling operand and refusal, and may
refuse a recursive change of an operand (`refuses`). `ViewRoot` is how a tool
refuses one of `/` (`--preserve-root`): by its spelling before anything is
looked up, and by its identity once a directory operand is held; its refusal
is GNU's two lines (`RootRefusal`), a link left alone GNU's `-v` line
(`untouched_link`). `OWNER_RW` and `OWNER_RWX` are the modes a node staged out
of anyone else's sight is born with.

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

`copy_attributes(source, destination, mode)` sets every attribute the held
node `source` lists on the held node `destination`. A key the source no longer carries
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

## Modes: `file_mode` and `DirModes`

What a copy makes takes its source's mode as GNU's `cp` gives it where it does
not preserve modes, less the umask at the make. `file_mode(source)` is a
file's: the source's permission bits, the set-ID and sticky bits dropped.
`DirModes::new(mode)` is a directory's, from its source's permission and
sticky bits (or the mode a new directory takes by default): it is made in
`asked()` — those bits with its owner's read, write and search, so its entries
can be made however the source or the umask withheld them — and
`settle(born)`, from the mode it was born with, answers `to_fill()`, the mode
to give it before it is filled when the umask withheld the owner's bits, and
`once_full()`, the mode it is left with: the bits asked less the umask, and a
setgid bit the directory it was made in passed down. Each answers only a
change, so a source the umask leaves alone costs no further call.

A move carries its source's mode and owner instead, as `mv` does:
`carry_owner(wanted, current, at)`, given the `Ownership` (kind, mode, owner
and group) of the source and of the node made, sets the bits first, while the
node is the mover's, then the owner; an owner the mover may not give stays the mover's, in
the source's group where it may keep that, without the set-ID bit of an owner
or group it did not keep. A link keeps no bits of its own.

## The report: `Omissions`

A tool notes what each node's copy left (`note`), gathers runs together
(`merge`, for a batch), and reports once at the end: `report(producer)`
answers a `Report` holding

- `summary`: one clause the tool frames for its own surface — `cp: warning:
  …` on `stderr`, `fstree`'s message line, the file manager's warning alert —
  naming how many files lost which keys (at most eight named, then "and
  others") and why, each key written by the shell-escape rule every name from
  elsewhere is written by (`tairix_util::quote`), quoted too where it holds a
  comma, so nothing a volume's own key carries — a control, a bidirectional
  override, a zero-width character, a comma passing for the list's own —
  reaches a surface as itself;
- `record`: one framed `stdinfo` `omission` record, code
  `fs.attributes_not_kept`, whose `ai` object lists the refusal codes, the
  file count, the keys named and whether that list is exhaustive.

## No copy of itself: `landing` and `Descent`

A tree copy whose destination lies inside its source would copy each new copy
again without end. `landing(source, dir)` answers before anything is written
whether the held directory `dir` — the target merged into, or the directory a
new target is made in — is `Apart` from the held directory `source`, the
`Same` directory, or `Within` it. It climbs from `dir` through `..`, comparing
each directory's identity with the source's until it reaches the root — the
directory that is its own `..`, or for a root with no identity the second
identity-less directory in a row — so neither a link, a `..` nor a second
spelling of a volume hides the source; a climb past the depth any path reaches
is refused as `LengthOutOfRange`. A
source whose volume offers no identity (`FileId::NONE`) is apart from
everything, and `Descent` still refuses the copy's own directories. A check
that fails says which side failed it (`Unresolved::Source` or
`Unresolved::Target`, `at` naming that path), so a refusal is reported against
the path that caused it.

`Descent` keeps the copy honest while it runs: `enter(source, made)` records
the source directory being copied and the directory made for it, `refuses(id)`
tells the walk a directory it is already inside, and `leave()` pops the level.
A directory moved into the source meanwhile, or a link leading back up the
tree, is therefore refused where the walk meets it. It holds two identities a
level, so it costs the walk's depth, never the tree's size. `cp`, `mv`,
`fstree` and the file manager's paste refuse through both.

## The operation: `op`

`op::Operation` is the one resumable copy, move and removal of a tree, for a
surface that must slice the work off its loop or stop on a question.
`Operation::transfer(verb, source, target, taken, stage)` copies or moves the
name `source` holds to the name `target` holds — each a `Named`, a name in a
directory the caller opened (`Named::parent` through a `Roots`) and the path
its user knows it by, a spelling for reports never resolved — and
`Operation::remove(source)` removes one. A source's `seen` is the node a
listing showed under the name, or `FileId::NONE` where none was listed: a name
that has come to hold another node is refused as `Changed` before anything is
changed. `stage`, a `Stager` (`tairix_rt::staging_name`), names the
replacement an overwrite builds beside the name it replaces. `advance(units,
buf)` carries out at most `units` units, each one entry acted on or one chunk
of a file's bytes through `buf`, which grows to at most `CHUNK_LEN` as a file
needs it, and answers `Done`, `Yielded` (units spent, work left), `Paused` or
`Failed`. Sliced a unit at a time it makes exactly the calls one unbounded
advance makes.

- **Reached by name.** The walk beneath a directory source is a `Cursor`; every
  entry is opened by name from a directory the operation holds, through no
  link, and changed through the node it was read from. A directory swapped for
  a link at any point, in the source or in the target a copy merges into,
  leads it nowhere: the paths an operation carries are only spellings for its
  reports. A volume attached inside a tree is listed with no identity of its
  own and is gone into as the volume it is.
- **A taken name.** Under `Taken::Ask` a leaf whose target name is taken pauses
  on a `Conflict` — the leaf, the name, and whether a file or a link holds it —
  until `resolve(Decision)`. `Overwrite` builds the replacement under a staging
  name beside the occupant and, once it is whole and on the medium, renames it
  over the occupant only while that is the node asked about (else the question
  is asked again about whatever took its place), so a failure on the way
  leaves the occupant as it was; a move on one volume is one replacing rename,
  bound to the occupant the same way. `Skip` leaves both, and `Cancel`, taken
  at any time, stops. A copy's directory meeting a directory merges into it; a
  move's replaces it only while it is empty, on one volume or across two, and
  is refused by a full one (`Rename`, `NotEmpty`). A leaf meeting a directory,
  or the reverse, fails as `KindMismatch`. Under `Taken::Refuse`, for a caller
  that chose free names, any taken name fails. A copy onto its source's only
  name fails rather than destroying it; one onto another name for the same
  node keeps the source whole.
- **A copy** gives what it makes `cp`'s modes (`file_mode`, `DirModes`), its
  attributes as far as the destination keeps them (noted in `omissions()`),
  and never copies a directory into itself (`landing`, `Descent`). Like `cp`
  it does not wait on the medium, but for an overwrite's replacement.
- **A move** is one rename, `NO_REPLACE` and `EXPECT_NODE`, on its own volume.
  Across volumes it is a copy that carries the source's mode and owner
  (`carry_owner`), put on the medium — its
  root, and any volume mounted inside it — after which only what it copied
  while unchanged is removed: an entry added or changed meanwhile is kept and
  listed in the report's `kept`, and a skip keeps the skipped source and the
  directories holding it. `removing()` says while it removes, work the
  report's counts do not include.
- **A removal** takes a directory's entries before the directory, a link as
  itself, each only while it is the node its listing read: an entry put in its
  place meanwhile is kept, and named.
- **Ending.** A copy that fails or is cancelled is wound down first: the file it
  was writing, or a replacement it was staging, is removed while it is still
  the one made, and each directory it made is given its mode. The first
  refusal ends an operation, what was done staying done; `rm` and `cp` carry
  past a refused entry, where a surface acting for a user stops and says which.
  The `Report` counts what was done (`done` entries, `bytes` accounted for,
  `skipped` leaves), whether it was cancelled, and what was kept. Each
  `OpError` names the entry it stopped at where there is one (`path()`). What
  grows with the tree — the levels it is inside, a move's record of what it
  copied, the names it kept — is allocated fallibly, so a heap that cannot hold
  it stops the operation as `Read` with `OutOfMemory`.

`fstree` drives an operation to its end or its question on its key loop; the
file manager runs one per item of a paste, a delete or a move to Trash on its
operations worker, a unit per step, under `Taken::Refuse` since its plan chose
free names.

## One run, one destination: `Created`

`mv a/x b/x d/` would put `b/x` over the `a/x` it had just moved there, whose
source is already gone. `Created` records the nodes a run's operands put in
place — each by identity with the name it was put at, held there under any
ASCII-case spelling a folding volume gives that name, and by path on a backing
that offers no identity — and `cp` and `mv` refuse a later non-directory
operand whose destination it holds, in GNU's words ("will not overwrite
just-created 'd/x' with 'b/x'"). Another name for such a node, a hard link
the run did not make, is replaced as GNU replaces it: the run's own name
keeps the node. A directory still merges into, or replaces the empty,
directory an earlier operand made. The record is a fallible hash map and set
under the process's keyed hasher, since a name can be a hostile volume's to
choose; a heap that cannot grow it stops the run rather than let an operand
through unguarded.

## Test support

The `test-support` feature carries `fixture::Tree`, an in-memory filesystem
behind the tree seam whose directory and file handles are bound to nodes as
the kernel's descriptors are. It keeps the kernel's rules a test can observe —
the creator's umask, a link never followed where the seam says so, the rename
rules, `NodeExpect` answering `Stale`, removed directories, volumes a rename
or link cannot cross, and the owner and permission checks its module docs
list, a mode change only by the node's owner whatever the caller holds —,
root spellings (`named_root`) resolved as the system resolves a volume or an
alias, nodes with no identity (`unidentified`), a capability gate kept as the
gate tools' doubles read and set it (`gated`, `gate`, `gate_of`, `set_gate`),
and an attacker hook (`before_call`, `before_line`) that runs between
two of its calls and may rename, replace or relink any name, so a test proves
what a race finds. `refuse` injects any other failure. `fixture::resolve` is
the kernel's path resolution in miniature — each link on the way followed,
each `..` applied, a cycle cut off as `LinkLoop` — for a test filesystem keyed
by path.

The crate is `no_std` + `alloc`, forbids `unsafe`, and depends on `lib/abi`,
`lib/util`, `lib/path`, `lib/collections` and `lib/hash`.
