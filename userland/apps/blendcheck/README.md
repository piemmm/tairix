# `tairix-blendcheck` — the display engine's blend beside the desktop's own

A command app registered at `/System/Commands/blendcheck.app/Run`. It takes
over the screen and shows a grey ramp under three panes of dark glass: the
first blended by the desktop with its ordered dither, the second blended by
the desktop rounded to nearest — what banding looks like — and the third
handed to the display engine as a layer of its own and blended by the
hardware. A key or a pointer button gives the screen back.

It is the metal comparison `plans/GPU.md` GD3 names: a display engine may
state `AccelCaps::band_free_blend` only where its pane looks like the first,
and the command prints what the engine's driver states beside the pattern so
the two can be checked against each other. Stability tier: `experimental`.

## Usage

```
blendcheck

  -h, -?         show this command's own short help
```

Run it from a text session: a desktop session holds the display, and the
command fails closed with that reason rather than waiting for it. A display
with no hardware layers has no engine blend to compare, and the command says
so.

## Exit status

- `0` — the pattern was shown and dismissed.
- `1` — the display was held, had no layers or refused the pattern, or the
  seat was lost while it was shown. A command line that was not understood
  exits `1` too.

## Design

`src/lib.rs` builds the pattern and is host-tested: its two desktop panes are
`lib/raster`'s own `blend_span`, dithered by `DitherRow::at` and undithered
by `DitherRow::NEAREST`, so the comparison is the compositor's arithmetic and
not a copy of it. `src/run.rs` owns the seat for as long as the pattern is
up: the lease, the frames the display driver allocated (`establish_display`),
one layer buffer, and a wait set on the seat's input, every channel of which
is drained on each wake so the wait parks rather than spins. Every exit after
the lease releases the seat back to the text console.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths; a refused allocation is a
stated failure.

## Required capabilities

All within the session baseline:

- `CAP_DISPLAY` — the seat's display lease and its configuration.
- `CAP_INPUT_READ` — draining the owned seat's keyboard, pointer and touch
  channels.
- `CAP_SHM` — mapping the frames and the layer buffer the driver grants.
- `CAP_CONSOLE_WRITE` — what it reports and why it stopped.
- `CAP_FS_ACCESS` — the short help, read from its own `Help/` tree.
