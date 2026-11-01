## NAME

mount — list the mounted filesystems, or mount a disk image

## SYNOPSIS

`mount`

`mount [option...] image [target]`

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

The volume stays mounted until you detach it with `unmount`, or your
login session ends. An image that holds a partitioned disk, one disk of
a RAID array, or a filesystem whose recorded owners are always honoured
(ext2, ext3, ext4, ARXFS) is not mounted, and `mount` says why.

## OPTIONS

- `-r, --read-only` — mount the volume read-only (as `-o ro`).
- `-w, --rw, --read-write` — mount the volume read-write, or not at
  all: an image you may not write, or whose filesystem cannot be
  written, is not mounted. A later `-r` takes this back; a later
  `-o ro` or `-o rw` does not. The last of `-r`, `-w`, `-o ro` and
  `-o rw` decides whether it is read-only.
- `-t, --types <list>` — the filesystems, comma-separated, one of which
  the image must hold, as the mount listing names them (`iso9660`,
  `udf`, `adfs`, `affs`, `fat32`), or `auto` for whatever it holds; a
  list beginning `no` names the ones it may not hold. What the image
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
- `-h, -?, --help` — show this command's own short help.

## EXAMPLES

- `mount` — list the mounted filesystems.
- `mount Workbench.adf` — mount an Amiga floppy image, at
  `/Storage/workbench` if that is its label.
- `mount -r disc.iso /Storage/disc` — mount a disc image read-only as
  `disc`.
- `mount -v -t adfs Elite.adl` — mount an Acorn floppy image, saying
  where.

## EXIT STATUS

- `0` — the listing was written, or the volume was mounted (or the
  short help was written).
- `1` — the command line was not understood.
- `2` — the mount listing could not be read, or the terminal could not
  be written.
- `32` — the image was not mounted.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
