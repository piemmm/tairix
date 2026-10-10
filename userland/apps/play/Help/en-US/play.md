## NAME

play — play sound files

## SYNOPSIS

`play [option...] file...`

## DESCRIPTION

Plays each file in turn on a sink. Every file is decoded in a sandboxed
worker that holds no authority at all, so a hostile file can at worst end its
own decode: it is left out with the reason stated and the rest of the list
plays. AU, WAV and FLAC files are read, FLAC native or in Ogg.

Consecutive files of one rate, sample format and channel layout play gapless,
in one stream. A file of another shape waits for what is queued to play out,
then opens a stream of its own.

When standard input is a terminal, `play` draws a full-screen interface: the
file playing, where it is, the level and a meter for each channel, and the
list. Playback does not depend on it. Sent to the background, `play` keeps
playing and gives the terminal back; brought to the foreground, it draws
itself again. Without the interface it reports a one-line progress on a
terminal's standard error.

The interface takes these keys: Space or `p` pauses and plays on; the left
and right arrows seek ten seconds; `n` or `>` goes to the next file; `b` or
`<` goes back to the start of this file, or to the one before within its
first three seconds; `+`, `=` or the up arrow is three decibels louder, and
`-`, `_` or the down arrow three quieter; `q` or Ctrl-C stops; Ctrl-Z
suspends, pausing the stream first.

A stream's level is attenuation: a stream cannot be raised past full scale,
so `--gain` and the level keys go no higher than 0 dB. To play louder, raise
the sink's volume.

A time is `[[HH:]MM:]SS[.fraction]` — `90`, `1:30`, `1:02:03` or `12.5`.

On standard information (fd 3) `play` writes a `schema` record for each file
it plays, an `omission` record for each file left out or cut short, and a
`summary` record when it ends.

## OPTIONS

- `-q, --quiet` — no interface and no progress line.
- `-v, --verbose` — each file's format and length on standard error.
- `--ui, --no-ui` — draw the interface, or never draw it; `--ui` without a
  terminal is refused.
- `-d, --device <sink>` — the sink to play on: `audio:sink/default`,
  `audio:sink/<id>` for this boot, or `audio:sink/<location>` wherever the
  device is, as `--list-devices` names them.
- `-g, --gain <dB>` — the stream's level, 0 or below, to the hundredth.
- `-s, --start <time>` — begin each file at this time.
- `-t, --duration <time>` — play this much of each file.
- `-l, --loop[=N]` — play the list N times in all, or without N for ever.
- `--list-devices` — name the sinks this session may play on, each by its
  id and its location, and exit.
- `-h, -?, --help` — show this command's own short help.
- `--version` — show the version, and exit.

## EXAMPLES

- `play song.wav` — play one file, with the interface on a terminal.
- `play -q intro.au song.wav &` — play a list in the background.
- `play -s 1:30 -t 20 song.wav` — play twenty seconds from a minute and a half in.
- `play -l3 -g -6 loop.wav` — play a file three times, six decibels down.
- `play --list-devices` — see the sinks.

## EXIT STATUS

- `0` — every file played, or playback was stopped with none left out.
- `1` — a file could not be played, or playback could not go on.
- `2` — the command line was not understood.

## ENVIRONMENT

- `TERM` — the terminal the interface draws for.
- `LANG` — the preferred locale for the short help (a BCP-47 tag such as
  `fr-FR`).

## SEE ALSO

- `man`
