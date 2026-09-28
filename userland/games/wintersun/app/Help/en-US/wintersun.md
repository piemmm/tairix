## NAME

wintersun — walk a procedurally generated world

## SYNOPSIS

`wintersun [--seed SEED | --reference-scene]`

## DESCRIPTION

Opens a desktop window onto a generated world: a top-down view of ground the
machine synthesises rather than ships, lit by a low sun that throws long
shadows down every slope.

The world runs from an ice sheet in the far north to rainforest past the
equator. Its climate follows the latitude, the prevailing winds and the
mountains in their way, so a desert lies where the rain does not reach and a bog
where the water cannot drain. Each world is the product of one number, its seed:
the same seed opens the same world on every machine.

Nothing about the world is stored as artwork. Each material the ground is made
of — ice, dune sand, dry grass, forest floor, granite — is a handful of numbers
the client turns into a texture as it draws, so the world looks the same on
every machine and takes almost no space on disk. Roads wear into what they cross
rather than sitting on top of it.

Ground that has not been generated yet is drawn as the gap it is and fills in
as it arrives. The client draws what it has rather than stopping to wait, so
the window keeps answering while the world catches up.

Arrow keys or `W`, `A`, `S`, `D` walk. Two held at once walk the diagonal
between them at the same speed, and opposing keys cancel. The view follows you
and stops at the edge of the world rather than sliding off it. Hills too steep
to climb and water too deep to wade turn you aside.

`+` and `-` move the view closer and further, through five steps between one
world cell across eight pixels and one across a hundred and twenty-eight.

`F11` takes the window fullscreen and returns it to whatever it was before, so
a maximised window comes back maximised. `Escape` restores it. `Q` leaves.

Every detail is drawn at its finest until you choose otherwise. The
*Settings…* row of the game's icon-bar menu opens its settings window, where
the quality is *Ultra*, every detail at its finest; *Basic*, every detail at
its plainest at the window's full size; *Custom*, your own choice of the
lighting, the shadows, the ground's texture and the scale the game renders at,
each on its own slider; or *Auto*. Moving a slider makes the choice *Custom*.
Your choice is kept for the next time you play.

On *Auto* the client eases detail off when frames have run late for a while —
the lighting first, then the shadows, then the scale it renders at — and gives
it back, a step at a time, as they recover. It judges over seconds rather than
single frames, so a moment of other work on the machine costs nothing and a
larger window does not send it to its plainest, and it never draws figures too
small to read.

A window larger than the software renderer can fill is drawn at up to
2560×1440 and scaled up to the window.

## OPTIONS

- `-h, -?, --help` — show this command's own short help.
- `--seed SEED` — open the world SEED names, a whole number from 0 to
  18446744073709551615. Without it the game draws a new seed and reports it on
  the standard information stream, descriptor 3, so the same world can be opened
  again. Not with `--reference-scene`, which is one fixed world.
- `--reference-scene` — draw the fixed reference scene and hold it still: one
  realm, cast and moment, identical on every machine, so a picture of the
  window can be checked against one drawn elsewhere. `F11` and `Escape` still
  resize the window; nothing else moves.

## EXIT STATUS

`0` when you leave. A non-zero status names its reason on standard error: the
world could not be generated, the window could not be opened, or the session's
event channel was lost.

- `2` — the command line was not understood.
- `87` — the reference scene could not be drawn.

## SEE ALSO

`sapper`
