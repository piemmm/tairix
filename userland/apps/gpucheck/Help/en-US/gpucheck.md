## NAME

gpucheck — check each served GPU, the screen showing what one draws, and a
kernel of your own

## SYNOPSIS

`gpucheck`

`gpucheck --present`

`gpucheck --shader FILE [--words N]`

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
answer by abandoning the drawing. It then asks the GPU to run a program
over more groups of work than it allows, which every device must answer
by abandoning that work. Last it runs small programs that between them
use every instruction a GPU program may, each answer held to the
software device's as closely as its instruction is defined to be exact;
a GPU that cannot run one of them is not held to it. A GPU agrees when
it leaves the same bytes as the software device, refuses that fill for
the same reason, abandons the same work, and answers each of those
programs alike.

It prints one line for each GPU, naming its hardware node: either that
the GPU agrees, or the first difference found — the work refused, and
at which step; a byte that reads differently; a different answer to the
fill; a value a graphics program left differently; a byte of a picture
that reads differently; a byte of a drawing that reads differently; a
different answer to the drawing from a list; a different answer to the
work past the GPU's limits; or an instruction of one of the small
programs answered further from the software device's answer than it is
defined to be.

`sysinfo gpu` lists the GPUs the command checks.

With `--present` it shows a known picture instead: a blue ground, a
band of colours, a row of checked tiles and one tile copied into the
far corner, in a window covering the whole screen. The first GPU a
driver serves draws it, or the software device when none is served. It
presents the picture once each frame until Escape is pressed or the
window is closed, then prints how many frames it presented, what drew
them, and whether the screen showed every one.

With `--shader` it runs a program of your own instead: the compute
kernel in FILE, written in WGSL or compiled to SPIR-V, told apart by
how the file begins. WGSL is compiled in a separate process that can
reach nothing else, and either kind is checked before any GPU is given
it. A kernel that is refused is not run: the refusal is printed, with
the line and column it lies at in WGSL. The kernel must have one
compute entry point, using one storage buffer at group 0, binding 0.
The buffer holds N words, 256 unless `--words` says otherwise, word *i*
starting as *i*, and the kernel runs once for each word. It runs on
every GPU a driver serves and on the software device; beside it, in
memory belonging to a program of its own, the GPU holds a marker the
kernel's buffer must never come to hold. For each GPU it prints either
that the two agree — the buffer reads the same, or both abandoned a
kernel that ran past its time limit and went on serving — or the first
difference: a word that reads differently, the marker found in the
buffer, the kernel refused, abandoned or never answered where the
software device ran it, or a GPU that served nothing once it had
abandoned it.

## OPTIONS

- `--present` — show the picture full screen, and check that the screen
  showed every frame presented.
- `--shader FILE` — run the compute kernel in FILE, WGSL source or a
  SPIR-V module, on every served GPU and on the software device.
- `--words N` — with `--shader`, how many words the kernel's buffer
  holds: from 1 to 65536, and 256 when not given.
- `-h, -?` — show this command's own short help.

## EXAMPLES

- `gpucheck` — check every served GPU.
- `gpucheck --present` — show the picture full screen; press Escape to
  stop.
- `gpucheck --shader double.wgsl` — run a WGSL kernel over 256 words on
  every served GPU.
- `gpucheck --shader kernel.spv --words 4096` — run a compiled kernel
  over 4096 words.

## EXIT STATUS

- `0` — every served GPU agreed with the software device; with
  `--present`, the screen showed every frame presented; with
  `--shader`, every served GPU agreed with the software device on the
  kernel.
- `1` — a GPU differed or could not be reached, no GPU is served, the
  served GPUs could not be listed, or the software device could not run
  the work; with `--present`, a frame was not shown, none was
  presented, or the window or the device refused a step; with
  `--shader`, the file could not be read, the kernel was refused, or a
  GPU differed. A command line that was not understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
