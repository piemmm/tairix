# Disk images

A user opens a disk-image file — an `.iso`, an Acorn or Amiga `.adf`, an
`.adl`/`.adm`/`.ads`, an `.hdf` — and the volume it holds appears at
`/Storage/<name>`, private to them, until they eject it or their login
session ends. The staged design is `plans/DISK-IMAGES.md`; the volume is
served from user space as every other volume is
([Volumes served from user space](./served.md)).

## What attaches

One file is one volume: a floppy, a disc image, a bare partition. The
kernel recognises it with the one probe every volume goes through
(`lib/fsprobe`); a file's name decides only that a double-click mounts it.
An Acorn `.adf` and an Amiga one are told apart by their contents: AmigaDOS
needs its `DOS` boot block and a root block whose type, secondary type,
hash-table size and checksum all hold, ADFS its map checksums and directory
markers.

ISO 9660, UDF, FAT32, ADFS and AmigaDOS ([AmigaDOS driver](./affs.md))
attach, every node presented as its attacher's whatever owners the format
records — Rock Ridge's and UDF's included — as a network share is
(`PresentedFs::whole`). ext2/3/4 and ARXFS are refused: their recorded
ownership is served as recorded on every volume ([drives](./drives.md) §20),
which a presentation for one principal would contradict. Everything else is
refused with an `ImageRefusal` naming why —
unrecognised, not the filesystem asked for, a RAID member, partitioned (an
Amiga Rigid Disk Block), its recorded owners always honoured (ext2/3/4, ARXFS),
damaged, a variant its driver does not read, unreadable — and every surface
states that reason (`ImageRefusal::reason`).

## `image_attach`

`image_attach` (syscall 164, `lib/abi/src/imageattach.rs`) takes a
descriptor and an `ImageAttach`: the catalog name wanted, the presentation,
its `ImageAccess`, a directory to bind it at too, the filesystem expected,
and whether an attach finding the volume there already takes it up for the
caller's session (`adopt`). The reply names the volume and says whether it is read-only; a
directory that would not take the volume, and a name asked that the volume
may not take — one the naming policy keeps, or one in use, found so even as
the volume is shown — are answered as such, with the reason
(`ImageAttachReply::TargetRefused`, `ImageAttachReply::NameRefused`), and
nothing is left attached. A C
program fills a `tairix_attach_head_t` and reads a
`tairix_image_attach_reply_t` ([C development header](../abi/c-abi.md)).

- **The descriptor is the authority.** The caller opens the image itself; a
  descriptor conferred or granted by another process is refused. It must be
  readable, by a caller holding `CAP_FS_ACCESS`; the volume is writable only
  when the descriptor is, the attach did not ask for it read-only, and the
  driver writes the format. An attach asking `Writable` gets a writable
  volume or is refused `ReadOnly` — the descriptor or the format will not
  write — and nothing stays mounted. The attachment holds a counted clone of the open
  file with the caller's identity frozen into it, so the file outlives the
  caller's close and an unlink.
- **One attachment per user and file.** Its identity is SHA-256 over a
  domain label, the attacher's uid, the file's volume and inode, and what the
  probe found; attaching the same file again answers `AlreadyAttached` with
  the volume there is. An attach that adopts it — the desktop's — hands it
  to its session too, so it lives until every login session it was handed
  to has ended, its teardown cancelled if one was owed; one that does not —
  `mount`'s — changes nothing, and is refused `Busy` for a volume owing its
  teardown rather than told of one about to leave. While another attach is
  answering for it, an operation holds it, or it is lost awaiting its
  owner's eject, the attach is refused `Busy`.
- **One writer.** The attachment holds an advisory lock over the whole file,
  exclusive when writable and shared when not ([locking](./locking.md)), so a
  second writable attach, or a cooperating writer, is refused `Busy`.
- **Presented as its attacher's.** `Private` — the caller and their primary
  group, `rwx------`/`rw-------` — for anyone; `Shared` and `Explicit` need
  `CAP_FS_MOUNT`. Always `noexec`. A target — for a caller without
  `CAP_FS_MOUNT`, an empty directory of their own under `/Users` — is bound
  in the same call.
- **Named from the volume.** An explicit name must be a label's own
  kebab-case spelling and no reserved word, so it can never take a name the
  policy gives a volume it names itself, else it is refused `OutOfRange`; it
  is taken exactly or refused `AlreadyExists`. Otherwise the volume's label,
  else the file's stem, else `virtual_<fingerprint>`, through the one naming
  policy.
- **Bounded.** At most 16 image volumes per user, each costing a driver
  process, judged as the volume enters, so a file attached already still
  answers its volume at the bound; an image whose file lies four image
  volumes deep is refused `LimitExceeded`.
- **It answers when the volume is served.** The kernel publishes the
  volume's node under its own image-volumes node, which no driver binds, and
  `devmgr` loads the format's driver bundle, as for a stick. Devices and
  principals draw node ids from one counter, but a principal's request is
  refused `NoSpace` once it passes the ceiling halfway through the ids left
  when the boot's tree was seeded, so the rest stay for devices and attaches
  cycled however fast never leave a device without one. The call
  waits, killable, 30 seconds for the driver to start and then on its
  liveness; a driver's refusal is answered as a reason, and a volume taken
  out before the call could answer is `NotAttached`.
- **An answer that cannot be delivered is undone.** The reply is written to
  the caller from within the attach: if it cannot be, a volume the attach
  mounted is taken out, and the hold it gave a login session on one attached
  already is let go, so no one holds a volume they were never told of.

The image's blocks are 512 bytes over the held file; a trailing partial
block is outside the volume, and a short read is a device fault, never
zeros. Its medium is virtual, of the form the probe found: a disc image is
an optical disc, a floppy's layout a floppy.

## Lifetime

- **Eject.** The owner detaches it without `CAP_FS_MOUNT`: flushed through
  its driver, then the file, then taken out of the namespace.
- **The login session's end.** The session notifier tells the volume service
  which sessions ended, and an image none of whose sessions is open comes to
  owe its detach. The notifier's tasks do it, as a forced detach would:
  flushed where it can be, files still open on it cut off, and a loss
  audited. No session's end waits on a flush, and no two of those tasks
  detach one user's images at once, so one user's slow device holds up only
  their own. One taken up by another of its user's sessions before then
  lives on; a detach that fails is tried again once something else holding
  an image lets go.
- **The volume the file lies on.** A plain detach of it while an image on it
  is attached is refused `Busy`; a forced one detaches those images first; a
  surprise removal or a lost server retires them with it.
- **The driver lost.** As for a stick: the stand-in answers, the mount
  reports recovering, and `devmgr`'s restart serves it again.

The mount table records an image volume as `VolumeClass::Image`, its owner
the attacher and its source the file's name. One presented to its attacher
alone — `Private`, or `Explicit` naming the attacher with no group or other
access — is shown to every other user without that source or its usage, and
none of the directories it is bound at but its catalog entry; one presented
to others too — `Shared`, or `Explicit` granting them access — is listed
whole ([System Information API](../abi/sysinfo.md)).

## Surfaces

- **Opening an image.** `lib/browse` types `.iso` as
  `application/x-cd-image`, `.adf`/`.adl`/`.adm`/`.ads` as
  `application/x-raw-floppy-disk-image` and `.hdf` as
  `application/x-raw-disk-image`, the parent of both; activating any is
  `Activation::MountImage`. `files.app` and the desktop carry it out on a
  worker of their own (`tairix_browse::mount::MountWorker`), since an attach
  waits for a driver, and open a window on the volume; a refusal is stated in
  an alert. *Mount Read-Only* and *Eject* are in `files.app`'s menus, and
  *Eject* on the places rail. *Mount Read-Only* of an image mounted writable
  already says so rather than opening the writable volume. An eject first
  has the app's reader read where the volume is bound — once, then, rather
  than with every change to the mount table — and takes `files.app`'s own
  windows off every place it is shown, its catalog entry and each directory
  it is bound at, to the catalog, letting go of what any window watches or
  was offered there; the reader then carries it out after the read under way
  and any write committed before it, so no read or watch of the app's own
  holds the volume as it goes; a copy the app is running on it still does,
  as it should.
- **`mount IMAGE [TARGET]`** mounts one from a terminal, and
  **`unmount NAME`** detaches it ([utilities](../userland/utilities.md)).

## Audit

| Id | Name | What happened |
|---|---|---|
| 4320 | `fs.image.awaiting` | published, awaiting its driver (`cause`: the presentation) |
| 4321 | `fs.image.attached` | served and mounted, or served again (`cause`: the format) |
| 4322 | `fs.image.attach_refused` | an attach was refused (`cause`) |
| 4323 | `fs.image.detached` | detached (`cause`, and what a force gave up) |
| 4324 | `fs.image.detach_refused` | a detach was refused (`cause`) |
| 4325 | `fs.image.retired` | retired with its file's volume, or its driver lost (`cause`, `outcome`) |
