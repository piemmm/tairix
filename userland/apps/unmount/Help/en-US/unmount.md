## NAME

unmount — detach a mounted volume

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Takes the volume mounted under `name` out of service: the filesystem
and the device are flushed, the mount under `/Storage` is retracted,
and the volume's durable `id::` root is withdrawn. `name` is the
volume's catalog name (`usb1`), a directory it is mounted on
(`/Storage/usb1`, or the one `mount image dir` bound it at), or the file
name of the disk image it was mounted from (`Workbench.adf`), matched
against the System Information API's mount and view listings; a file
name another user's image shares names your own. A file name more than
one volume could be meant by, or one the listing keeps only the start
of, names none: name the volume by where it is mounted.
A path may be relative or end in a slash: it is resolved as `mount`
resolves one. Another user's private volume lists neither its file name
nor the directories it is bound at, so it is named by where it is
mounted in `/Storage`.

A volume whose device was removed while it still held uncommitted
writes stays visible as `unavailable-dirty` (or `unavailable-lost`)
in the mount listing, and a plain `unmount` refuses: its retained
data is held for a verified re-insert. A volume with files still open
on it, or that a program has as its working directory, refuses a plain
`unmount` too. `--force` is the deliberate exit — files still open on
the volume are cut off, a program working in it is left in a directory
that is gone, the retained data is discarded, the volume is retracted,
and what was given up is recorded in the audit log. On a healthy volume `--force`
still flushes and detaches cleanly; nothing is discarded when a
clean commit is possible.

A disk image or network share you mounted yourself is yours to
detach; any other volume needs the mount authority (`CAP_FS_MOUNT`).
The kernel checks it and audits every decision. The permanent boot
volumes and the system's view bindings are not detachable.

## OPTIONS

- `-f, --force` — force-unmount: retract the volume even while files
  are open on it, a program is working in it, or its uncommitted data
  cannot be committed, cutting the open files off and discarding the
  retained data.
- `-h, -?, --help` — show this command's own short help.

## EXAMPLES

- `unmount usb1` — cleanly detach the volume mounted as `usb1`.
- `unmount /Storage/usb1` — the same, named by its mount point.
- `unmount Workbench.adf` — detach the disk image mounted from
  `Workbench.adf`.
- `unmount --force usb1` — retract an unavailable volume, discarding
  its retained uncommitted data.

## EXIT STATUS

- `0` — the volume was detached (or the short help was written).
- `1` — the command line was not understood, or the caller may not
  detach the volume.
- `2` — the system information service, or writing to the terminal,
  failed.
- `32` — the volume was not mounted or is not detachable, the path
  naming it could not be resolved, or its detach failed.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `mount`
- `df`
- `man`
