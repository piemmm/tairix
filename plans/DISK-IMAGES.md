# DISK-IMAGES.md — Mounting disk-image files

Binding under `AGENTS.md`. A user double-clicks a disk-image file — an
`.iso`, an Acorn or Amiga `.adf`, an `.adl`/`.adm`/`.ads`, an `.hdf` — and
the volume it holds appears at `/Storage/<name>`, private to them, until the
file manager's last window onto it closes while nothing else uses it — what
the user's settings choose by default — or until they eject it or their login
session ends. It consumes and never contradicts
`plans/FSSERVE.md` (the attach, serve and detach the volume goes through),
`plans/FOREIGN-MEDIA.md` (the disc formats), `docs/src/filesystem/drives.md`
(`Storage:`, presentations) and `plans/FILES-INTERACTION.md` (activation).

## Ledger

| id | What it is | Status |
|---|---|---|
| DI1 | Recognition in `lib/fsprobe`: the AmigaDOS recogniser (DOS\0–\7, the checksummed root block), the Amiga Rigid Disk Block, and the form a volume was laid out for (disc, floppy) | done |
| DI2 | `drivers/filesystem/affs`: OFS and FFS, international and directory-cache volumes read; DOS\0–\3 written; hard links read, made and promoted; soft links read and made | done |
| DI3 | The kernel image attachment: `image_attach` over a held descriptor, the image block, the probe and its refusals, the per-principal identity, presentation and lock, owner eject, the login session's end, the backing volume's departure | done |
| DI4 | The disk-image content types, `Activation::MountImage`, the floppy, disc and hard-disc icons | done |
| DI5 | `files.app`: mount on activation on a worker of its own (`tairix_browse::mount::MountWorker`), the desktop's own opens handed to it, the window onto the volume, *Mount Read-Only*, *Eject* on the rail and in the menus | done |
| DI6 | The `mount` bundle — `mount IMAGE [TARGET]` and the listing — and `unmount` of an owned image volume, by name or by its file's (closes D1014 for image sources) | done |
| DI7 | The QEMU vertical (`tairix-test-images-qemu-aarch64`): an ADFS, an Amiga and an ISO 9660 image copied into a home, mounted with `mount`, read, written where writable and refused where not, ejected with `unmount`, and read back once mounted again | done |
| DI8 | Writing directory-cache Amiga volumes (DOS\4/\5): keeping each directory's cache blocks in step | planned |
| DI9 | Long-filename Amiga volumes (DOS\6/\7, AmigaOS 3.2) | planned |
| DI10 | Partitioned images: the Amiga Rigid Disk Block's partitions, an MBR or GPT disk image | planned |
| DI11 | Writing an AmigaDOS volume whose bitmap is not validated: rebuilding the bitmap from the tree, as AmigaDOS's validator does, rather than opening it read-only | planned |
| DI12 | A private volume's catalog name shown only to its owner: `/Storage` projected per principal, so another user's listing names no image or share they cannot open, and the mount and view lists leave it out rather than withheld | planned |
| DI13 | Image-volume node ids issued again: a node identity that carries its generation, so a withdrawn node's id can be reissued without one device's proofs speaking for another, and the requested half of the id space can no longer be spent | planned |
| DI14 | A QEMU vertical of the desktop's own mount and eject: an image double-clicked in `files.app` opens its window, and an eject from the rail with windows on the volume — at its catalog entry and at a bound directory — is not refused for the app's own holds | planned |
| DI15 | The AmigaDOS bitmap kept to the working set: bitmap pages and the structural blocks checked against them read and held as a change needs them, not two volume-sized lists for the volume's lifetime | planned |
| DI16 | Per-principal volumes at scale in the kernel's registries: the mount table's children indexed by their parent projection, the driver registry by handle, and the image registry by served id, volume id, owner, name, backing handle, own handle and server, with the login sessions each volume holds and the volumes owed a teardown indexed too, so a path walk, a file operation, an attach, a claim's release and a session's end cost no pass over every principal's volumes | planned |
| DI17 | A mount listing that waits on no driver and reads the table once: usage kept current off the listing's path by drivers that count it, and `sysinfod` sizing its read from the kernel's answer, so neither a slow image's driver nor the number of volumes stalls every principal's listing | planned |
| DI18 | An incremental hardware-tree read: the nodes changed since a generation, so neither the copy under the tree's lock nor `devmgr`'s decode and match spans every image node in the system | planned |
| DI19 | Bounce buffers sized by transfers, not volumes: staging borrowed from a pool reserved per nesting level, so memory follows the transfers in flight and no transfer waits on a buffer one above it holds | planned |
| DI20 | Partitioned removable devices: the volume manager plans the partitions an Amiga Rigid Disk Block lays out on a device, as it plans a PC table's, rather than attaching none | planned |
| DI21 | An image the user opened unmounted once no window shows it: `DetachMode::Idle`, the file manager's opened set, the `files.unmount_when_closed` setting | done |
| DI22 | A per-attachment identity in the image attach reply and the idle detach, so an unmount the user did not ask for can only take the attachment the file manager made | planned |
| DI23 | Retitling an owned image volume: its label written through `FilesystemLabel` and `fsserve-v1`'s `Label`/`SetLabel` by `VolumeService::retitle`, for its owner alone; the mount record's title; the syscall and `files.app`'s *Title* row | done |

## 1. What an image is

- **One file, one volume.** The whole file is the volume: a superfloppy, a
  disc image, a bare partition. The kernel recognises it with the one probe
  every volume goes through (`lib/fsprobe`); a file's name decides only that a
  double-click mounts it. Telling an Acorn `.adf` from an Amiga one is the
  probe's: AmigaDOS needs its `DOS` boot block and a root block whose type,
  secondary type, hash-table size and checksum all hold; ADFS its map
  checksums and directory markers. Neither is ever taken for the other.
- **What attaches.** ISO 9660, UDF, FAT32, ADFS and AmigaDOS, every node
  presented as the attacher's whatever owners the format records (Rock
  Ridge's and UDF's among them), as a network share is. ext2/3/4 and ARXFS are
  the formats whose recorded ownership is served as recorded on every volume
  (drives.md §20), which a presentation for one principal would contradict:
  refused, `RecordsOwners`.
- **Refusals are reasons.** `ImageRefusal` is a closed set — unrecognised,
  not what was expected, a RAID member, partitioned, records owners, damaged,
  an unsupported variant, unreadable — so every surface states why.

## 2. `image_attach` (`lib/abi/src/imageattach.rs`)

- **The descriptor is the authority.** The caller opens the image itself and
  passes the descriptor; one conferred or granted by another process is
  refused. It must be readable and the caller must hold `CAP_FS_ACCESS`, as
  for `fs_read`; the volume is writable only when the descriptor is too, the
  attach did not ask for it read-only, and the driver writes the format.
  `ImageAccess::Writable` asks for a writable volume or none: a read-only
  descriptor, or a first serve that would mount it read-only, is refused
  `ReadOnly`. `expects` is a set of filesystems (`VolumeFsTypes`, the frame's
  own word) the probe's answer must fall in, else `NotExpected`: a claim held
  to recognition, never a choice of driver. The reply says whether the volume is read-only; a target
  directory that would not take the volume is answered `TargetRefused`, and an
  asked name the volume may not take — kept by the naming policy, or in use,
  judged again as the volume is shown — `NameRefused`, each with its errno and
  nothing left attached (a volume mounted before the directory was tried is
  taken out before the reply, unless the attach is being killed or the volume
  is held; §4), so a caller names the directory or the name and never the
  image. The attachment keeps a counted clone of the description, acting as the
  attacher's identity frozen at the attach, so the file outlives the
  caller's close and an unlink. It is held through the projection its file's
  volume was attached as, never a bind it was opened through, so a bind
  unbound, or withdrawn with a volume it stood in, takes nothing from it.
- **One attachment per principal and file.** The identity is derived:
  SHA-256 over a domain label, the attacher's uid, the file's volume identity
  and inode, and the identity the probe found — so two copies of one image,
  or an image and its disc, attach side by side, and attaching the same file
  again answers `AlreadyAttached` with the volume it has. An attach that
  adopts it (the desktop's) hands it to its login session too, cancelling a
  teardown owed, so it lives until every session it was handed to has ended
  — but only a volume as writable as it asks (`ImageAccess::accepts`): a
  read-only attach meeting a writable volume is answered as one that does
  not adopt. One that does not (`mount`'s) changes nothing, and is refused
  `Busy` for a volume owing its teardown. One that another attach is answering for, that
  an operation holds, or that is lost awaiting its owner's eject is refused
  `Busy`.
- **Writers are kept apart.** The attachment takes an advisory lock over the
  whole file, exclusive when writable and shared when not, owned by the
  attachment and released at its detach: a second writable attach, or a
  cooperating writer, is refused `Busy`. Locks are advisory (FILELOCK
  decision 2); a program that writes the file without one may still corrupt
  its own image, never anything else.
- **Presented as its attacher's.** *Private* — owner the caller, its primary
  group, `rwx------`/`rw-------` — for anyone; *shared* or *explicit* with
  `CAP_FS_MOUNT`; every node presented whole (`PresentedFs::whole`). Always
  `noexec`. A target directory is a `view_bind` made under the same call.
- **Named from the volume.** An explicit name must pass
  `tairix_fsprobe::name::chosen` — a label's own kebab-case spelling and no
  reserved word, so never a name the policy generates for another volume —
  or is refused `OutOfRange`; it is taken exactly or refused `AlreadyExists`,
  reserved under the registry's guard as the volume enters. Otherwise the
  naming policy's candidates for the label, else the file's stem, else
  `virtual_<fingerprint>`.
- **Bounded.** A principal holds at most `IMAGE_VOLUMES_PER_PRINCIPAL` (16)
  image volumes, each costing a driver process, judged as the volume enters,
  so a file attached already still answers `AlreadyAttached` at the bound; an
  image whose file lies on image volumes nested `IMAGE_NESTING_MAX` (4) deep
  is refused `LimitExceeded`. Containment bounds, fixed (§24.4).
- **It answers when the volume is served.** The kernel publishes the volume
  node under the kernel's image-volumes node, which no driver binds, so no
  driver's departure takes it; `devmgr` loads the format's bundle exactly as
  for a stick. The node is a principal's request, so its id comes from the
  lower half of the ids left once the hardware tree was seeded
  (`HwTreeStore::publish_requested_child`): ids are never reissued, and
  attach cycling can spend only that half, never the devices' (DI13). The
  call parks, killable, until the driver serves or refuses the volume;
  before a driver touches it the wait is bounded by `IMAGE_DRIVER_START_NS`,
  after that by the driver's liveness, as every served call is. A driver's
  refusal is answered as a reason; a volume taken out before the call could
  answer is `NotAttached`.
- **The answer is delivered within the attach.** `VolumeService::attach_image`
  hands its reply to the syscall's copy-out (`AnswerSink`); one that cannot be
  delivered is undone — a volume it mounted is abandoned, and the session
  hold it added to one attached already is let go — so a caller is never
  left holding a volume it was not told of. While an attach answers for a
  volume, no other takes it up and no settling task takes it down.

## 3. The image block (`kernel/tairix-kernel/src/image_volume.rs`)

- 512-byte blocks over the held description; the trailing partial block, if
  any, is outside the volume. A short read (the file shrank) is a device
  fault, never zeros. `flush` syncs the file's volume. Its medium is virtual,
  with the form the probe found: a disc image is an optical disc, an image laid
  out for a floppy a floppy.
- The served-volume registry holds image volumes beside the device volumes,
  so `volume_read`/`write`/`ctl` reach either through one lookup.
- An archive is an image attachment of another shape
  (`kernel/tairix-kernel/src/image_archive.rs`): read by byte to its exact
  length rather than by block, with work files and a commit that replaces
  its file; its design is `plans/ARCHIVES.md` §2.

## 4. Lifetime

- **Eject.** The owner detaches it without `CAP_FS_MOUNT`, like a network
  volume: flushed through its driver, then the file, then retracted. A lost
  volume is taken only by a force or by a detach that takes the lost
  (`DetachMode::Lost`), which the kernel forces only for a volume still
  recorded lost, decided under its claim, and is a plain detach otherwise —
  so a stale view of a lost volume whose identity a healthy one has taken
  since cannot force the healthy one. A forced take-down's audit records its
  outcome `forced`.
- **Owed teardowns.** An attach that gives up once its volume is mounted —
  its target refused, its answer undeliverable — takes the volume down itself
  with a plain detach before it returns, so nothing it did not report is left
  attached. A teardown the task finding it owed may not wait on — the
  session's end, which the reaper finds; an attach killed meanwhile, which
  waits on no flush; one whose volume an operation holds, perhaps its own
  driver in its serve; a take-back's plain detach that was refused — is never
  done by that task. The volume is marked owed (`Owed::Orphaned`,
  `Owed::Abandoned`) and the session notifier's tasks
  (`session_reaper::NOTIFIER_TASKS`) settle it, one volume a task and never
  two of one owner's at once, so one principal's slow device holds up only
  its own. A volume taken up by an attach before it is settled owes nothing;
  a settling that fails leaves it stuck until something else holding an
  image volume lets go.
- **The login session's end.** The notifier tells the volume service which
  sessions ended; an image none of whose sessions is open comes to owe its
  detach, done as a forced detach is, flushed where it can be, files still
  open on it cut off, a loss audited.
- **The backing volume leaving.** A plain detach of the volume holding an
  attached image is refused `Busy` (`image_attached`); a force detach detaches
  the images on it first; a surprise removal or a lost server retires them
  with it, stand-in style, nothing waiting on a server.
- **The driver lost.** As for a stick: the stand-in answers, the mount reports
  recovering, the node stays published and `devmgr`'s restart serves it again.
  A plain eject meanwhile detaches it as a stick's detach does, nothing being
  left to flush (`Writes::Gone`).
- **An idle detach.** `DetachMode::Idle` is a plain detach a surface asks for
  once the user has finished with a volume: refused `Busy` or `NotEmpty` while
  anything holds it, which `DetachMode::left_in_use` reads as an answer rather
  than a refusal, audited at `Info` rather than `Warn`. It never forces, and
  never takes a lost volume.

## 5. The mount table

`MountRecord` names an image volume `VolumeClass::Image`, its owner the
attacher, its source the file's name, at most `MOUNT_SOURCE_MAX`, and marks
it `private` when it is presented to the attacher alone. `sysinfod` lists
such a volume to every other principal withheld — without its source or
usage, marked so — and a binding of it only where it is its catalog entry,
without the subtree shown; no capability widens either. Its catalog name is
still listed in `/Storage` to everyone (DI12).

The record's `title` is the volume's own label (DI23): the probe's as it
attaches, cut between characters to `VOLUME_TITLE_MAX`, then what its owner
retitles it. `VolumeService::retitle` takes the owner alone, holds the volume
as any operation does, has its driver write the title without reading the one
it replaces — the driver answers for its format, so one keeping no title says
so before a volume taking no writes is refused — and lists the new title once
the driver holds it, its writeback publishing it should the flush fail; no
format's identity takes in its label, so a retitled volume is still the
attachment it was, and the catalog name stays while the volume is
attached. It is withheld with the source. The file manager's icon-bar slot
offers *Title* on a titled image of the user's own: the row's field opens on
the title with the caret at its end, choosing the row opens it, and a commit
is carried out on the mount worker (`MountAct::Retitle`), never on the loop; an
emptied or over-long title is refused before the kernel is asked.

## 6. AmigaDOS (`drivers/filesystem/affs`)

- Every structure is checksummed and validated on the way in; a mismatch is
  `BadMagic`. Block numbers are bounded by the volume; hash chains, extension
  chains, OFS data chains and link chains are length-bounded, so a cycle ends.
- DOS\0–\5 are read; DOS\6/\7 (long filenames) are refused `Unsupported`.
  DOS\0–\3 are written; DOS\4/\5 open read-only, `UnsupportedFeature`, since
  their directory caches are not kept (DI8). A volume whose bitmap is not
  validated opens read-only, `NeedsCheck`, as AmigaDOS would revalidate it.
- Names are ISO 8859-1 on disk and UTF-8 above, at most 30 bytes;
  international volumes fold as `NameMatching::Latin1CaseInsensitive`, the
  others as `AsciiCaseInsensitive`, the fold one definition with the hash.
- Node ids are header block numbers: stable, the root's its own. A removed
  held node keeps its blocks until its last unpin, and the root stays invalid
  while one does or its free failed there, so AmigaDOS reclaims them.
- Metadata: `amiga.protection` (the raw low byte) and `amiga.comment`; times
  are AmigaDOS date stamps (days since 1978, minutes, ticks), local time read
  as UTC, as FAT's are.
- A change's first write marks the bitmap invalid; the commit flushes the
  bitmap before marking it valid, and leaves it invalid while a held object
  has lost its last name, so a crash is revalidated by AmigaDOS. A refused
  change writes nothing. Hard links are one node under
  several names; removing an object's own name promotes its newest link to
  take its place, as AmigaDOS does. Soft links read and are stored in
  AmigaDOS's spelling; a hard link to a directory reads as a soft link, so the
  tree stays a tree.

## 7. The desktop

- `lib/browse` types `.iso` as `application/x-cd-image`, `.adf`, `.adl`,
  `.adm`, `.ads` as `application/x-raw-floppy-disk-image` and `.hdf` as
  `application/x-raw-disk-image`, the parent of both; their icons are the disc,
  floppy and hard-disc pictures. Activating any of them is
  `Activation::MountImage`.
- `files.app` carries a mount out on a worker of its own,
  `tairix_browse::mount::MountWorker` over the runtime's one queued worker
  (`tairix_rt::work::Worker`), since an attach waits for a driver: never on the
  loop, and never behind the listings on its other workers. An image opened on
  the desktop is handed to it (`OpenTarget::Image`, or `files --mount IMAGE`
  when none is running), so one program attaches every image the user opens
  and is the one to let it go. The
  image is opened read-write where the user may write it (read-only for *Mount
  Read-Only*); the volume opens in a window on `/Storage/<name>`, and a refusal
  is stated in an alert in the one wording `MountDone::shown` gives both. The
  rail and the context menu offer *Eject* for an image or network volume the
  user owns. An eject first moves `files.app`'s own windows on every place the
  volume is shown (`tairix_browse::mount::shown_at`: its rail row and each
  view binding, `Projection`) to the catalog, giving up a navigation onto it
  and letting go of what they watch or were offered there
  (`Watches::release_within`); the reader serving the volume's lane then
  carries the eject out (`tairix_browse::mount::carry_out`) after the read under
  way there, so no read of the app's own routed to the volume overlaps it — one
  routed before the mount table was re-read can still hold it, and the eject is
  then refused as busy, saying so — and with no reader the loop does, as it
  reads then. A lost volume is no rail row but keeps its catalog entry, whose
  *Eject* asks first (`tairix_browse::mount::lost_eject_question`, the
  window's one question) and then sends `DetachMode::Lost`. *Mount
  Read-Only* of an image mounted writable already is refused, saying so,
  never answered with the writable volume or taken up for the session; one
  of an image no path can name says why (`MountAct::read_only_of`), as Open
  does.
- **Unmounted once closed (DI21).** A volume an attach of `files.app`'s made
  (`MountDone::attached`: only a fresh `Attached`, never one already attached)
  is noted in its opened set (`userland/apps/files/src/opened.rs`). Each turn
  of the loop, where `files.unmount_when_closed` holds and a read of the
  user's settings has said so, a noted volume no window shows, describes a
  node of, is opening onto, or runs an operation reaching (a progress window,
  or one still starting) is asked an idle detach (`EjectAsked::Finished`), one
  at a time: no window is moved off it, and one a window has come to show by
  the time it is located is withdrawn (`EjectDesk::withdraw`) and the places
  it was found shown at are learned, so it is asked again once that window
  leaves. No idle detach is asked while an attach is on its way, and an attach
  asked for while one is waits for its answer, so the two never race. Found in
  use, it is asked again at once when a child the file manager started exits
  or a window shows it and leaves it again, and otherwise after a wait that
  doubles from `FIRST_RETRY_NS` to `LONGEST_RETRY_NS`, on the loop's one-shot
  deadline, since nothing says when a holder elsewhere lets go. A lost volume
  is left for its owner's own *Eject*; one the user ejects, and one unmounted
  some other way once the mount table no longer lists it, is forgotten. An
  image the shell unmounts and mounts again before the mount table is re-read
  keeps its identity, so the opened set would take that attachment for its
  own; a per-attachment identity in the attach reply and the idle detach
  closes that (DI22, planned).

## 8. The command line

`mount [-lrvw] [-t LIST] [-o ro,rw,noexec,loop,name=NAME] IMAGE [TARGET]`: the
source is an image file (util-linux's implied loop), opened read-write where
the user may write it; `-w` asks for it read-write or not at all, as
util-linux's does, until a later `-r` takes it back. TARGET `/Storage/NAME` names the volume, any other
directory is bound too, resolved and bounded first and refused as itself —
the kernel answers a directory that would not take the volume
`TargetRefused`, and an asked name it may not take `NameRefused` — and a
`name=` the naming policy keeps is a name no volume may take (exit `1`, with the
hint to `--help`); one in use refuses the mount as `/Storage/NAME`. `-t` is a
util-linux type list, each element a filesystem as the mount table names it, a
leading `no` naming the ones excluded, or `auto`; it is the attach's `expects`,
the set the probe's answer must fall in, so the order chooses nothing; with no operands it is a util-linux type
list the listing is filtered by, and `-v`/`-l` list as without them. Silent on
success; `-v` says where it went; a volume mounted read-only unasked is warned
of, as util-linux warns of a write-protected source. Exit `32` for a mount
refused, an unknown type and `already mounted` — naming where it was asked to
go and where it is — included, each failure a sentence ending in a full stop,
printed with no hint to `--help`. `unmount NAME` detaches an image volume its
owner names by its catalog name, a directory it is bound at, or its file's,
a file name two users' images share resolving to the caller's own and one two
of either could mean, or the listing keeps only the start of, naming none; an
empty NAME is a usage error, `-h` is the short help, and it exits as
util-linux's `umount` does: `1` for misuse or a detach it may not make, `2` for
a failed service, `32` for an unmount that failed. A device or volume source waits on D1014's discovery
surface.

## 9. Remaining work

- **DI8.** A directory-cache block lists each entry's header, size,
  protection, date, type, name and comment; every create, remove, rename,
  resize and attribute change rewrites the affected record and the chain's
  checksums.
- **DI9.** Long names and comments share the header's name area, a comment
  that does not fit moving to a comment block.
- **DI10.** Attaching one partition of a partitioned image: the RDB's
  partition list, or `lib/partition`'s MBR/GPT walk, naming the extent.
- **DI11.** Validating a volume walks every header, extension and data block
  from the root, marks each in a fresh bitmap, and writes it with the root's
  flag set, refusing a tree it finds damaged. It is also what finds a bitmap
  naming a live object as one of its pages, or marking one free, and a data
  block two tables name — two of one file's, or two files' — which no read of
  a single block can tell from the truth: until then a cut or removal frees
  such a block while another table still names it.
- **DI12.** The catalog's listing filters a volume attached for one principal
  — an image, a private share — to that principal and the mount authority,
  and a lookup of its name by anyone else answers `NotFound`, the namespace
  projected per caller rather than shared. `MOUNT_LIST` and `VIEW_LIST` then
  leave such a volume out rather than list it withheld: until then every
  listing carries one withheld record per other principal's private volume,
  so a consumer that bounds what it reads — the Switchboard keeps 256 volume
  records — can spend its bound on volumes it cannot show and leave out the
  reader's own.
- **DI13.** `HwNode`'s identity widens to an id and a generation, every
  consumer — the driver store's grants, the DMA quarantine's proofs, `devmgr`'s
  bindings, `hw_tree_wait` — matching both, so the store can reissue a
  withdrawn image node's id. Until then the requested half bounds what attach
  cycling can spend, and image attaches, not devices, are what run out.
- **DI14.** The desktop vertical drives `files.app` through the seat the way
  the GUI verticals drive the session: activate an image, wait for its
  window, open a second window on a directory it is bound at, eject from the
  rail, and require the audited detach rather than a refusal. Until then the
  decisions it composes are host-tested in `lib/browse` (`shown_at`,
  `within`, `holds_within`, `Watches::release_within`, `Browser::navigate_to`,
  `MountAct::eject_chosen`, `MountAct::read_only_of`) and in `files.app`'s
  `question` module, and the loop that strings them together is not.
- **DI15.** `drivers/filesystem/affs`'s `Bitmap` holds every bitmap-block
  pointer and every structural block it checks a change against for the
  volume's life: about 4 MiB each for a 2 TiB volume of 512-byte blocks, in
  the driver's own process. Pages loaded on demand under the reclaim model,
  with the structural check made against the headers a change reaches, bound
  it by the working set instead, keeping the invariant that the root and the
  bitmap's own blocks are named once and never written or freed as anything
  else.
- **DI16.** `MountTable::child` and `beneath` search every projection, and
  `LateFilesystem` every registered driver by handle, under the locks every
  path walk and file operation takes; each image volume adds one of each, so
  with many principals' images a walk under `/Users` pays a pass over all of
  them. The image registry is a vector in served-id order searched linearly
  under its spin lock: by volume id (`holds`, `attached_as`, `reply_for`,
  `detach_image`), owner (`owned_by` and `enter`'s bound), name (`reserves`,
  `enter`), backing handle (`backed_by`, which every detach and retirement
  asks first), its own handle (`depth_on`) and server (`image_server_lost`),
  with removal a memmove. Every claim's release runs `settleable_in`, two
  passes; every session's end anywhere runs `held_sessions` (two passes, an
  allocation of every held session, a sort) and `end_sessions` (a third). An
  index of each projection's children by name (exact, with the case-folding
  volumes' names folded into their own key), a driver map keyed by handle,
  the image registry's indexes on each key above, an index from each login
  session to the volumes it holds — with the reaper telling the observer
  which sessions ended, a full pass only past a bounded queue — and the set of
  volumes owed a teardown make each lookup `O(log N)` and each telling and
  release proportional to what it touches, kept in step under the locks that
  already guard each structure.
- **DI17.** `mount_snapshot` asks each idle volume's driver in turn for its
  usage (`ServedFs::stats`, a round trip that parks until the driver answers),
  so one listing waits on the sum of their answers, figures most callers are
  then withheld included. The image drivers differ: FAT32, ISO 9660 and UDF
  answer from counters they keep; AFFS scans its bitmap on the first call;
  ADFS reads its whole free-space map on every call. `sysinfod` serves one request at a
  time, so that wait holds every principal's queries — `ps`, `df`, the rail,
  `fstree`'s free-space figure on every directory change — and any user may
  attach `IMAGE_VOLUMES_PER_PRINCIPAL` images to lengthen it.
  `sysinfod`'s `read_whole` starts at 64 records and doubles, each attempt a
  fresh introspection that runs the whole snapshot again (`log2(N/64) + 1`
  snapshots of `N` volumes), an unheld page walk reads the list again for
  every page, and `LateFilesystem::entries` copies every entry, two string
  allocations each, under the drivers' spin lock. Each image-format driver
  answers `Stats` from counters it keeps (AFFS and ADFS computing theirs as
  they mount and maintaining them), the proxy keeps the volume's last usage,
  refreshed as writes commit, and the snapshot reads that without asking a
  driver; the introspection answers the length it needs, so one read sizes
  the buffer, and the entries are copied with no allocation under the lock.
- **DI18.** `hw_tree_read` copies the whole tree under the store's spin lock,
  which every publish, resource lookup and wait-set scan takes — the buffer is
  allocated before it — and `devmgr` decodes and matches all of it whenever
  the tree moves; a move of the mount table alone re-reacts over the tree as
  last read. A read of the nodes changed since a generation — added,
  withdrawn, re-marked — bounds the copy and each reaction by those.
- **DI19.** `ServedVolume::new` reserves and zero-fills `BOUNCE_LEN` for every
  served volume, used only while a transfer holds the volume's disk lock. An
  image volume's transfer holds its buffer while `ImageBlock::read_blocks`
  reads the file, and that read is the backing volume's own transfer — a
  device's or, nested up to `IMAGE_NESTING_MAX`, another image's — so one
  shared pool a transfer waits on would deadlock once outer transfers held
  every buffer. Buffers reserved per nesting level, a transfer at depth `d`
  borrowing only from level `d`'s share and waiting (parked, never spinning)
  only on its own level, bound the memory by the transfers in flight while no
  transfer ever waits on a buffer a transfer above it holds.
- **DI20.** `drivers/storage/volmgr` recognises a Rigid Disk Block and counts
  the device unrecognised. Planning it walks the RDB's partition list under a
  bound — the one reader DI10 walks for an image — and probes each partition's
  extent by content, as a PC table's partitions are; a readable PC table
  written over the disk's start still wins, and a device holding neither is
  never offered for an array as blank.
