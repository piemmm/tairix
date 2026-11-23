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
buffer, which every device must refuse. A GPU agrees when it leaves the
same bytes as the software device and refuses that fill for the same
reason.

It prints one line for each GPU, naming its hardware node: either that
the GPU agrees, or the first difference found — a byte that reads
differently, the work refused, or a different answer to the fill.

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
