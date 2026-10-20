# tairix-patience

Stability tier: **experimental**.

`patience` — the TAIRiX desktop card games: Klondike drawing one or three,
FreeCell, and Spider with one, two or four suits, with a solver behind every
hint. The design is documented in `docs/src/desktop/patience.md`.

The crate is a host-tested `[lib]` plus the freestanding `Run` binary of the
on-disk `patience.app` bundle. Everything with behaviour lives in the library;
the binary composes it over the window channel and three workers.

## Modules

- `card`, `variant`, `deal` — cards, the six games and their pile shapes, and
  the numbered deals (FreeCell numbered as Microsoft's are).
- `table` — the rules: every move, its consequences, and its exact undo.
- `play` — a game in progress: its line of play, grouped undo and redo, the
  score, and the line as stored text.
- `solver` — a bounded depth-first search over canonical positions, answering
  won with a line, lost, or unknown.
- `analysis` — what the workers ask the solver: a position's verdict, the
  last winnable step of a lost line, a deal it can win.
- `score`, `stats`, `prefs`, `saved`, `store` — scoring, the statistics, the
  preferences, the kept game, and the one document all of them are written to.
- `layout`, `art`, `motion`, `paint` — where everything is drawn, the card
  tiles, every card's movement, and the frame.
- `menu` — the commands, the two menus that offer them, and the alerts.
- `game` — the composition the `Run` binary drives; `input` routes the
  pointer, the keyboard and the menus into it.

## Authority

`CAP_CONSOLE_WRITE` and `CAP_SHM`, nothing else. The store is reached by an
unprivileged call gated on the bundle's kernel-attested identity.

## Testing

`cargo test -p tairix-patience` covers the rules and their undo over many
seeded games, Microsoft's FreeCell deals, every line the solver claims played
out to a won table and every loss it claims, the rewind's search, the stored
document against the shared fake app-data service, the layout at several
sizes and densities, the card art, every animation and its deadline, the
painter's clipping and reduced-motion frames, the menus, and the composed
game's pointer, keyboard, deal, win and resume paths.
