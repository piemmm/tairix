## NAME

label — show or change a volume's title

## SYNOPSIS

`label [option...] name [title]`

## DESCRIPTION

Shows the title of the volume `name` names or, given `title`, sets it.
A volume's title is the label its filesystem records: the name it is
shown by, and the name it is given in `/Storage` when it is next
attached. `name` is the volume's catalog name (`usb1`), a directory it
is mounted on (`/Storage/usb1`), the file name of the disk image it was
mounted from (`Workbench.adf`), or its durable root (`id::…`), named
as for `unmount`. An empty `title` clears the title where the volume's
format allows it.

While the volume stays attached its name in `/Storage` does not change,
so nothing naming it there stops working; the new title names it from
its next attach on. Its identity, and its files', stay as they were.

Each format spells a title its own way, and a title it cannot store is
refused rather than shortened. A FAT volume takes up to eleven
upper-case letters, digits, spaces and the punctuation a short file
name allows, never starting or ending with a space; an ext2, ext3 or
ext4 volume up to sixteen bytes; an ADFS volume up to ten characters;
an AmigaDOS volume one to thirty; a UDF volume up to 126; and an ARXFS
volume up to 252 bytes, unless it is encrypted. A disc mastered with a
title, an archive and a network share take no new one.

A disk image you mounted yourself is yours to retitle. A removable disk
— a USB stick, a card — is yours to retitle while you are using the
computer itself, if you may change what is in its top folder; any other
volume needs the mount authority (`CAP_FS_MOUNT`), which an
administrator has. The kernel checks it, and records every decision in
the audit log.

## OPTIONS

- `-h, -?, --help` — show this command's own short help.
- `--version` — show this command's name and version.

## EXAMPLES

- `label usb1` — show the title of the volume mounted as `usb1`.
- `label usb1 HOLIDAY` — title it `HOLIDAY`.
- `label /Storage/backup 'Backup 2026'` — title the volume mounted on
  `/Storage/backup`.
- `label usb1 ''` — clear its title.

## EXIT STATUS

- `0` — the title was shown or set (or the help or version written).
- `1` — the command line was not understood, the name named no volume,
  the title was not one, or the volume refused it.
- `2` — the system information service, or writing to the terminal,
  failed.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `mount`
- `unmount`
- `df`
