## NAME

audioctl — list the sound devices and change their controls

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

Lists the sinks and sources this session is shown: each one's id for this
boot, whether it is its direction's default, its level and mute, the rate it
runs at, the frames it has lost, its location and its name. `streams` lists
your own streams, and with `--all` every principal's.

`default`, `level`, `mute` and `unmute` change one device's controls. A
device is named by an `audio:` reference: `audio:sink/default` or
`audio:source/default` for the default now, `audio:sink/<id>` for this boot,
or `audio:sink/<location>` wherever the device is, as the listing names them.
A level is decibels to the hundredth, 0 or below, such as `-6` or `-3.5`; a
negative level needs no `--`.

A device's controls belong to the room it serves. The session holding that
room may change them, anybody may while the room is unclaimed, and nobody may
while it is withheld; a refusal says so. What a session sets is its own: while
another session holds the room they stand aside, and they are back when it
returns. The level every device starts at and the devices preferred as the
defaults are the machine's settings `audio.level`, `audio.output` and
`audio.input`, which `configure` sets.

On standard information (fd 3) `audioctl streams` writes an `omission` record
when it lists only your own streams.

## OPTIONS

- `-a, --all` — with `streams`, every principal's streams; this needs `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — show this command's own short help.
- `--version` — show the version, and exit.

## EXAMPLES

- `audioctl` — list the sinks and sources.
- `audioctl level audio:sink/default -10` — set the default sink ten decibels below full.
- `audioctl mute audio:source/default` — mute the default source.
- `audioctl default audio:sink/2` — make sink 2 the default.
- `audioctl streams --all` — list every principal's streams.

## EXIT STATUS

- `0` — the command completed.
- `1` — it was refused, or could not be carried out.
- `2` — the command line was not understood.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such as `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
