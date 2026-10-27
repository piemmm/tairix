## NAME

blendcheck — compare the display engine's blending with the desktop's own

## SYNOPSIS

`blendcheck`

## DESCRIPTION

Takes over the screen and shows a grey ramp across its width with three
panes of dark glass over it, one above another:

1. blended by the desktop, dithered, as the desktop draws translucent
   windows;
2. blended by the desktop without the dither, which shows the steps
   banding leaves in a smooth gradient;
3. blended by the display hardware itself.

If the bottom pane looks like the top one, the display hardware blends
translucency without banding. If it shows the steps of the middle one,
it does not. The command also prints whether the display's driver
states that its blending is band-free.

Press any key or pointer button to give the screen back.

The command needs the display to itself, so run it from a text
session rather than from a terminal on the desktop. A display with no
hardware layers has no blending of its own to compare, and the
command says so.

## OPTIONS

- `-h, -?` — show this command's own short help.

## EXAMPLES

- `blendcheck` — show the comparison until a key is pressed.

## EXIT STATUS

- `0` — the comparison was shown and dismissed.
- `1` — the display was held by a session, has no hardware layers,
  refused the pattern, or the seat was lost while it was shown. A
  command line that was not understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
