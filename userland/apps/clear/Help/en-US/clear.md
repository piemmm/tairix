## NAME

clear — clear the terminal screen

## SYNOPSIS

`clear [-x]`

## DESCRIPTION

Writes the sequence that moves the cursor to the top-left corner and
erases the whole display, leaving an empty screen. Which sequence is
written is decided by the terminal named in `TERM`; a terminal that
cannot clear (an unknown `TERM` degrades to the dumb baseline) makes
the command fail rather than print bytes the terminal would render as
garbage.

Where the terminal can, the lines it keeps above the screen — the
scrollback — are erased too, so nothing that was on the screen can be
scrolled back to.

## OPTIONS

- `-x` — keep the scrollback: erase only the screen.
- `-h, -?` — show this command's own short help.

## EXAMPLES

- `clear` — clear the screen.

## EXIT STATUS

- `0` — the clear sequence was written.
- `1` — the terminal cannot clear, or the output could not be
  delivered. A command line that was not understood exits `1` too.

## ENVIRONMENT

- `TERM` — the terminal whose clear sequence is written.
- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `reset`
- `man`
