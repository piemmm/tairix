# Sound controls (`audioctl`)

`audioctl` lists the sound devices and the streams on them, and changes a
device's controls, from a terminal or a headless machine (`plans/SOUND.md`
SND15). Its commands and options are its own Help document; this page is how
it is built.

## The service decides who may change a control

`default`, `level`, `mute` and `unmute` resolve their `audio:` target against
the devices the audio service lists now, then send `SetDefault`, `SetLevel` or
`SetMute` through `tairix_audio::stream::set_control`. The tool holds no
capability for this: `audiod` admits a control for the login session holding
the room the device serves, for anybody while the room is unclaimed, and for
nobody while it is withheld (`docs/src/userland/audiod.md`). A refusal ends the
command with its reason on standard error and exit status 1.

## Listings

The devices come from the audio service itself, walked by id with
`tairix_audio::stream::devices`, so a device lost during the walk costs it only
that device. The streams come from the System Information API: the caller's
own through `SELF_AUDIO_STREAMS`, and every principal's through
`GLOBAL_AUDIO_STREAMS`, which needs `CAP_SYSINFO_GLOBAL` and is refused with
that reason without it. Each row is `lib/procinfo`'s, the same one
`sysinfo audio` prints, so the two tools never spell a device two ways.

A level is typed as `tairix_audio::volume::typed_level` reads it: decibels to
the hundredth, 0 or below, with an optional `dB`. An operand that is a `-`
followed by a digit is a number, so a negative level needs no `--`.
