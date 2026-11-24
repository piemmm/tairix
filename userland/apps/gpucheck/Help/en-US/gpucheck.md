## NAME

gpucheck — check each served GPU against the software device

## SYNOPSIS

`gpucheck`

## DESCRIPTION

Runs the same short piece of work on every GPU a driver serves and on
the system's software device, and says for each GPU whether the two
agree.

The work writes a known pattern into memory the command can read, fills
the memory beside it, copies both through memory the GPU keeps for
itself and back again, and then asks for a fill past the end of a
buffer, which every device must refuse. It then runs each of the
system's own graphics programs on a known picture. Where the GPU can
hold every kind of picture the next step uses, it copies pictures in
and out of memory and between one another, clears them, and runs a
program that reads, writes and samples pictures. Where the GPU can
draw, it draws shapes, points and a line of known colours into
pictures, hiding what lies behind, leaving out shapes facing away,
blending colours and keeping some untouched, and gathers four samples
of each pixel into one. It also draws from a list in memory that names
the first copy of a shape to draw, which a GPU that cannot do that must
answer by abandoning the drawing. Last it asks the GPU to run a program
over more groups of work than it allows, which every device must answer
by abandoning that work. A GPU agrees when it leaves the same bytes as
the software device, refuses that fill for the same reason, and
abandons the same work.

It prints one line for each GPU, naming its hardware node: either that
the GPU agrees, or the first difference found — the work refused, and
at which step; a byte that reads differently; a different answer to the
fill; a value a graphics program left differently; a byte of a picture
that reads differently; a byte of a drawing that reads differently; a
different answer to the drawing from a list; or a different answer to
the work past the GPU's limits.

`sysinfo gpu` lists the GPUs the command checks.

## OPTIONS

- `-h, -?` — show this command's own short help.

## EXAMPLES

- `gpucheck` — check every served GPU.

## EXIT STATUS

- `0` — every served GPU agreed with the software device.
- `1` — a GPU differed or could not be reached, no GPU is served, the
  served GPUs could not be listed, or the software device could not run
  the work. A command line that was not understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
