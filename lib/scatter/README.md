# tairix-scatter

Stability tier: **experimental**

Keyed scatter: one place offered to each cell of a lattice fixed to the land,
thinned so no two kept crowd each other, the higher ranked standing. The
desktop's ray tracer (`lib/raytrace`) thins its woods by it, `lib/countryside`
its villages, and WinterSun's world (`userland/games/wintersun/world`) its
scatter, so the three share one definition of the rule. `no_std` + `alloc`,
and `#![forbid(unsafe_code)]`.

## What it provides

- `Offers` — what a consumer brings: each cell's offer, a pure function of the
  cell; its rank, two alike told apart by their cells; whether one offer
  stands within another's room; how many cells apart two offers can crowd
  each other; and a bound over a set of offers that says how far any of them
  could crowd.
- `Thinning` — a region, given as its rows of cells, kept or dropped over a
  number of rounds a bounded batch of work at a time, shared across a
  `tairix_parallel::JobRunner` in pieces, each offer read once over the region
  and the halo about it. It holds what it has read, never the offers, so
  offers may borrow what changes between calls.

## Guarantees

- **Rounds, bounded reach.** In each round an offer no undecided outranking
  offer crowds is kept and every undecided offer a kept one crowds yields;
  what is still undecided after the last yields. One round is Matérn's second
  process; enough rounds keep what thinning one by one from the highest
  ranked keeps, and fewer keep a subset of it. A cell's fate depends only on
  the offers within `2 × rounds − 1` reaches of it.
- **Pieces match the whole.** A region thinned in pieces keeps exactly what it
  keeps thinned whole within each piece.
- **Deterministic.** What is kept, and the order it is handed back in, is the
  same however the work's batches fall and whichever runner shares them.
- **Fallible.** Every row read and every wave's verdicts are taken fallibly;
  a refusal leaves the thinning as its last whole read or wave left it.

## Tests

`cargo test -p tairix-scatter`: a thinning keeps what a search over every pair
within reach keeps, offers ranked alike yielding to the later cell; no two
kept offers crowd each other, and some are thinned; rectangles and widening
spans thinned as pieces keep what the whole keeps there, over one round and
over several; a thinning keeps the same on one core as across several, rows
wider than a piece and runs of narrow rows alike; round by round it keeps
more of what thinning one by one keeps and nothing else, until enough rounds
keep all of it; and rows out of order, running off the lattice, or asked of
no rounds or too many are refused.

The design is in `docs/src/lib/scatter.md`.
