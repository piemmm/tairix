# tairix-scatter

`lib/scatter` holds the one rule places scattered over a land are spaced by:
each cell of a lattice fixed to the land offers at most one place, and of two
offers that crowd each other the higher ranked stands. The desktop's ray
tracer (`docs/src/lib/raytrace.md`) thins its woods by it, `lib/countryside`
(`docs/src/lib/countryside.md`) its villages, and WinterSun's world its
scatter (`plans/WINTERSUN.md` WS2). It is `no_std` + `alloc` and forbids
`unsafe`.

## The rule

A consumer implements `Offers`:

- **An offer** is a pure function of its cell, the place jittered within it and
  whatever stands there drawn from a key of the cell's own.
- **A rank** orders offers by what matters to the consumer — a tree's height,
  a village's draw. Two ranked alike are told apart by their cells, the one in
  the later row, or in the same row the later column, outranking, so of two
  offers that crowd each other exactly one outranks the other.
- **Crowding** says whether one offer stands within another's room. It may
  weigh the pair — a tree beneath a much taller one needs less room — or only
  the yielding offer's own, as WinterSun's scatter does.
- **The reach** bounds how many cells apart, on either axis, two offers that
  crowd can lie.
- **A bound** summarises a set of offers — for a wood, the most any keeps
  apart, the broadest crown and the tallest tree — and says whether an offer
  could crowd any of them so many cells away. A consumer with nothing finer
  than its reach uses `()`.

The rule runs in rounds. In each, an offer that no still-undecided outranking
offer crowds is kept, and every undecided offer a kept one crowds yields;
whatever is still undecided after the last round yields too. One round is
Matérn's second process: an offer yields even to an outranking one that yields
in turn, which keeps fewer and, where ranks are heights, mostly the tallest.
Enough rounds keep exactly what thinning the offers one by one from the
highest ranked keeps, each kept unless one kept before it crowds it. Over
`rounds` rounds a cell's fate hangs on the offers within `2 × rounds − 1`
reaches of it alone, so a region thinned in pieces keeps exactly what it keeps
thinned whole. The countryside's villages and WinterSun's scatter run one
round; the ray tracer's woods run four, which settles nearly all of a wood
(`docs/src/lib/raytrace.md`).

## Thinning a region

`Thinning` takes the region as its rows of cells, each a span of columns, rows
increasing: a rectangle, or a view's widening cone and the round about an eye.
It reads each row the spans within their halo of it need, across the columns
they hold and the halo either side, every offer once. Rows are carried through
their rounds in waves: a row goes through its next phase — a round's keeping,
then its yielding — once every row within reach of it has come as far, and a
wave decides its rows from the fates the rows about it held before it, which a
round's decisions never change the answer of. A span is ruled on once its row
has been through every round.

Each row keeps, for every strip of eight cells, the highest rank among its
still-undecided offers and their bound, and the bound of those its last
keeping kept, so a search for an outranking neighbour passes over a strip none
of whose offers could outrank or reach, and a search for a kept one over a
strip that kept none within reach.

Each call does about a batch of cells' work for each of the runner's cores,
sharing it out in pieces of a few hundred cells however few rows it takes, so
a row wider than a core's share keeps every core busy. It goes on with the
rows read before reading more, so it holds no more rows than a span's fate
hangs on and a batch beyond, and hands back what it kept in its rows' and
columns' order, the same however the work was shared. A heap that refuses a
row's room, or a wave's, ends the call; the thinning stands as its last whole
read or wave left it, and goes on from there when called again.

A thinning holds only the offers and ranks it has read, never the `Offers`
that read them, so a consumer's offers may borrow what it changes between
calls — the ray tracer's borrow its stage — so long as every call is handed
offers that answer alike.
