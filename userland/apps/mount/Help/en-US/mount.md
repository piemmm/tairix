## NAME

mount — list the mounted filesystems, or mount a disk image or archive

## SYNOPSIS

`mount`

`mount [option...] image [target]`

`mount --list-held`

`mount --replay-held name`

`mount --discard-held name`

## DESCRIPTION

With no operands, lists the mounted filesystems, one
`source on target type fstype (options)` line per place a volume is
shown. `-t` then lists only the types its comma-separated list names; a
list beginning `no` lists every type it does not name.

With an `image` operand, mounts the volume that disk-image file holds:
an ISO 9660 or UDF disc (`.iso`), an Acorn ADFS or AmigaDOS floppy
(`.adf`, `.adl`, `.adm`, `.ads`), an AmigaDOS hard-disk partition
(`.hdf`), or a FAT32 volume. What the image holds is recognised from its
contents, never from its name. The volume is mounted for you alone, at
`/Storage/name`, named from the volume's label or else the image's file
name; nobody else may open it. It is mounted read-write where you may
write the image and its format can be written, and read-only otherwise,
which `mount` warns of. A volume mounted from an image never runs
programs.

A `target` of `/Storage/name` mounts the volume under that name. Any
other `target`, an empty directory of your own under `/Users` or on a
volume in `/Storage`, shows the volume there as well; a directory on a
FAT32 or ADFS volume cannot show it.

An archive mounts the same way, its members the volume's files and
folders: a zip (`.zip`), a tar, plain or compressed with gzip or bzip2
(`.tar`, `.tar.gz`, `.tgz`, `.tar.bz2`), a single file compressed with
gzip or bzip2 (`.gz`, `.bz2`), a RISC OS Spark or ArcFS archive (`.spk`,
`.arc`), or a RAR archive (`.rar`). RISC OS file types and attributes
travel with each member. Changes are saved by replacing the archive file,
so an archive is mounted read-write only where you may write both the
file and the folder it is in. A RAR archive is always mounted read-only.
An archive split into several volumes, or one whose names are encrypted,
is not mounted, and an encrypted member cannot be read.

The volume stays mounted until you detach it with `unmount`, or your
login session ends. An image that holds a partitioned disk, one disk of
a RAID array, or a filesystem whose recorded owners are always honoured
(ext2, ext3, ext4, ARXFS) is not mounted, and `mount` says why.

A disk put back while it still held changes not yet written to it is
mounted read-only, the changes kept, when its device cannot show that it
is the same disk and not a copy; the listing marks it
`[replay-unconfirmed]`, and `--list-held` lists only such volumes.
`--replay-held` writes the changes to it, if its contents show it
unchanged since; `--discard-held` gives them up and takes the disk as it
stands. Either way the volume is mounted again, read-write where it can
be. Answering needs the mount authority (`CAP_FS_MOUNT`); the desktop
also asks the user at the computer about a removable disk.

## OPTIONS

- `-r, --read-only` — mount the volume read-only (as `-o ro`).
- `-w, --rw, --read-write` — mount the volume read-write, or not at
  all: an image you may not write, or whose filesystem cannot be
  written, is not mounted. A later `-r` takes this back; a later
  `-o ro` or `-o rw` does not. The last of `-r`, `-w`, `-o ro` and
  `-o rw` decides whether it is read-only.
- `-t, --types <list>` — the filesystems, comma-separated, one of which
  the image must hold, as the mount listing names them (`iso9660`,
  `udf`, `adfs`, `affs`, `fat32`, `zip`, `tar`, `tar.gz`, `tar.bz2`,
  `gzip`, `bzip2`, `spark`, `arcfs`, `rar`), or `auto` for whatever it
  holds; a list beginning `no` names the ones it may not hold. What the image
  holds is recognised from its contents, so the order chooses nothing:
  a disc holding both UDF and ISO 9660 is mounted as UDF, unless its
  ISO 9660 keeps Rock Ridge or RISC OS records. An image holding none
  of them, or a list naming no type a disk image holds, is not
  mounted.
- `-o, --options <list>` — a comma-separated list of `ro`, `rw`
  (read-write where it can be, the default), `noexec`, `loop`, and
  `name=name`, the name the volume is to take. `noexec` and `loop` are
  what an image is mounted as anyway.
- `-v, --verbose` — say where the volume was mounted.
- `-l, --show-labels` — accepted for the listing, which carries no
  labels to add.
- `--list-held` — list only the volumes holding changes that wait for
  `--replay-held` or `--discard-held`.
- `--replay-held <name>` — write the changes a volume holds to its disk,
  if its contents still show it unchanged; `name` is the volume's name in
  `/Storage`, a directory it is shown at, or its `id::` root.
- `--discard-held <name>` — give up the changes a volume holds, taking
  its disk as it stands.
- `-h, -?, --help` — show this command's own short help.

## EXAMPLES

- `mount` — list the mounted filesystems.
- `mount Workbench.adf` — mount an Amiga floppy image, at
  `/Storage/workbench` if that is its label.
- `mount -r disc.iso /Storage/disc` — mount a disc image read-only as
  `disc`.
- `mount -v -t adfs Elite.adl` — mount an Acorn floppy image, saying
  where.
- `mount Photos.zip` — mount a zip archive, read-write where you may
  write it and its folder.
- `mount -r backup.tar.gz /Storage/backup` — mount a compressed tar
  read-only as `backup`.
- `mount --list-held` — list the volumes whose changes wait for an
  answer.
- `mount --replay-held backup` — write the changes `/Storage/backup`
  holds to its disk.

## EXIT STATUS

- `0` — the listing was written, the volume was mounted, or its held
  changes were written or given up (or the short help was written).
- `1` — the command line was not understood, or you may not answer for
  the volume's changes.
- `2` — the mount listing could not be read, or the terminal could not
  be written.
- `32` — the image was not mounted, or the volume's changes were not
  written or given up.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
