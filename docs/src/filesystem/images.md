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
its `ImageAccess`, a directory to bind it at too, the filesystems expected
(`VolumeFsTypes`, a set the recognised one must fall in, never a choice of
driver), and whether an attach finding the volume there already takes it up for the
caller's session (`adopt`). The reply names the volume and says whether it is read-only; a
directory that would not take the volume, and a name asked that the volume
may not take — one the naming policy keeps, or one in use, found so even as
the volume is shown — are answered as such, with the reason
(`ImageAttachReply::TargetRefused`, `ImageAttachReply::NameRefused`), and
nothing is left attached: a volume mounted before its directory was tried is
taken out before the reply, or by the system soon after where the caller is
being killed or the volume is in use, a retry being refused `Busy` until then.
A C
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
  caller's close and an unlink. It holds the file through the file's own
  volume, so opening it through a bind ties nothing to that bind.
- **One attachment per user and file.** Its identity is SHA-256 over a
  domain label, the attacher's uid, the file's volume and inode, and what the
  probe found; attaching the same file again answers `AlreadyAttached` with
  the volume there is. An attach that adopts it — the desktop's — hands it
  to its session too, so it lives until every login session it was handed
  to has ended, its teardown cancelled if one was owed, but only a volume as
  writable as it asks: a read-only attach meeting a writable volume is
  answered as one that does not adopt. One that does not — `mount`'s —
  changes nothing, and is refused `Busy` for a volume owing its teardown
  rather than told of one about to leave. While another attach is
  answering for it, an operation holds it, or it is lost awaiting its
  owner's eject, the attach is refused `Busy`.
- **One writer.** The attachment holds an advisory lock over the whole file,
  exclusive when writable and shared when not ([locking](./locking.md)), so a
  second writable attach, or a cooperating writer, is refused `Busy`.
- **Presented as its attacher's.** `Private` — the caller and their primary
  group, `rwx------`/`rw-------` — for anyone; `Shared` and `Explicit` need
  `CAP_FS_MOUNT`. Always `noexec`. A target — for a caller without
  `CAP_FS_MOUNT`, an empty directory of their own under `/Users` or inside a
  volume in `/Storage`, reached through directories of their own or the
  system's, and never on a FAT32 or ADFS volume, whose nodes keep no identity
  a bind can hold — is bound in the same call, as any view bind is
  ([drives](./drives.md)).
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
  mounted is taken out before the call returns, or soon after where the
  caller is being killed or the volume is in use, and the hold it gave a login
  session on one attached already is let go, so no one holds a volume they
  were never told of.

The image's blocks are 512 bytes over the held file; a trailing partial
block is outside the volume, and a short read is a device fault, never
zeros. Its medium is virtual, of the form the probe found: a disc image is
an optical disc, a floppy's layout a floppy.

## Archives

An archive attaches through the same call and holds to every rule above;
its members are served by the archive driver ([Archive driver](./archive.md)).
What differs is how it is read and written:

- **Its exact length.** The volume's description carries the file's byte
  length (`VolumeInfo::byte_len`) and the name it was attached by
  (`VolumeInfo::archive_name`), so a driver finds a record at the very end
  and names a lone compressed file from its archive.
- **Written by replacing it.** A writable archive is attached with a
  descriptor of the directory its file lies in and the file's name there
  (`ImageAttach::file_name`), which must name that file; without them, or
  with a directory reached through a read-only view, it attaches read-only.
  The driver never writes the archive. Its first change makes two work files
  beside it, as the attacher and under the name every staged replacement
  takes: the staging file, unnamed once made, and the next archive, named
  until the commit. A work file is only ever cut, and written from no further
  than its end. The kernel copies unchanged ranges between them itself
  (`VolumeCtl::Copy`).
- **The commit.** `VolumeCtl::Commit` makes the next archive durable, gives
  it the file's security record and attributes, and renames it over the
  file's name in the held directory — only while that name still names the
  attached file, else the commit fails and the archive is unchanged. A
  replace the volume stopped partway is settled: finished where the old name
  is gone, or the next archive kept under its own name, never lost with the
  old. A directory that cannot be made durable after the rename is the next
  flush's answer. A crash leaves the old archive whole. Work files never
  committed are let go, the next archive taken back out of the directory, as
  the attachment ends or a driver serves it in place of one lost; where the
  file's volume is gone they go with it.

## Lifetime

- **Eject.** The owner detaches it without `CAP_FS_MOUNT`: flushed through
  its driver, then the file, then taken out of the namespace.
- **Lost.** A volume whose backing went with writes unpublished stays listed,
  unreadable, until its owner ejects it with a force, or with a detach that
  takes the lost (`DetachMode::Lost`): the kernel forces that one only while
  the volume is recorded lost, and takes any other volume down as a plain
  detach, so a stale view never forces a healthy volume. Its audit says
  `forced`.
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
  reports recovering, and `devmgr`'s restart serves it again. A plain eject
  meanwhile detaches it, as a stick's detach does: what the driver held
  unpublished went with it, so there is nothing left to flush.

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
  *Eject* on the places rail. A lost volume has no rail row, and *Eject* on
  its catalog entry asks before letting what was unsaved go, then sends
  `DetachMode::Lost`. *Mount Read-Only* of an image mounted writable already
  says so rather than opening the writable volume or holding it for the
  session, and one of an image no path can name says why. An eject first
  has the app's reader read where the volume is bound — once, then, rather
  than with every change to the mount table — and takes `files.app`'s own
  windows off every place it is shown, its catalog entry and each directory
  it is bound at, to the catalog, letting go of what any window watches or
  was offered there; the reader serving the volume's lane then carries it out
  after the read under way there and any write committed before it, so no read
  or watch of the app's own routed to the volume holds it as it goes — one
  routed before the mount table was re-read can, and the eject is then refused
  as busy, saying so; a copy the app is running on it still does, as it
  should.
- **`mount IMAGE [TARGET]`** mounts one from a terminal, and
  **`unmount NAME`** detaches it ([utilities](../userland/utilities.md)).

## Audit

| Id | Name | What happened |
|---|---|---|
| 4320 | `fs.image.awaiting` | published, awaiting its driver (`cause`: the presentation) |
| 4321 | `fs.image.attached` | served and mounted, or served again (`cause`: the format) |
| 4322 | `fs.image.attach_refused` | an attach was refused (`cause`) |
| 4323 | `fs.image.detached` | detached (`cause`, `outcome` `clean` or `forced`, and what a force gave up) |
| 4324 | `fs.image.detach_refused` | a detach was refused (`cause`) |
| 4325 | `fs.image.retired` | retired with its file's volume, or its driver lost (`cause`, `outcome`) |
| 4326 | `fs.image.archive_commit` | an archive commit left work undone (`archive`, `cause`: `next_kept`, the next archive kept under its own name after a replace the medium stopped partway; `record_stale`, a record taken during the commit not carried; `not_durable`, the directory not yet durable; `next_left`, an uncommitted next archive not taken back out) |
