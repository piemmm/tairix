# tairix-countryside

`lib/countryside` lays out the countryside of a region with no notion of how it
is drawn. It lays out:

- the holdings the land is farmed in;
- the villages and farmsteads on them;
- the ways between them;
- the fields the land between ways and water is cut into;
- each field's boundaries, gates and use;
- the plots of a farmstead's yard and a village's streets.

The desktop's ray tracer (`docs/src/lib/raytrace.md`) sets these out as
geometry. WinterSun (`plans/WINTERSUN.md` WS30, WS35) is to set them out as
world objects. The staged design is `plans/RAYTRACE.md` RT13. The crate is
`no_std` + `alloc` and forbids `unsafe`.

## Holdings and settlements

- **Holdings.** A holding is a cell of a jittered triangular lattice: the land
  about one vertex, out to the middles of the six triangles about it. Each
  corner is the middle of three vertices summed in whole millimetres, so
  neighbouring holdings share their corners to the bit and tile the land.
- **Villages.** One is offered in every square of a coarser lattice. It stands
  where its ground suits it, unless a better-ranked offer lies within its
  exclusion. Rank comes from the square alone, so a village depends on the
  squares about it and no further.
- **Farmsteads.** A holding no village gathers has its own farmstead, on the
  best ground near its middle. A farmstead is laid out as a yard squared to the
  contour or to the way it faces. Its house stands at one end of the front with
  a garden before it, its barn across the yard, and its byre and sheds as its
  plan has them: courtyard, ell or loose.
- **Village plots.** A village's plots line the streets that pass it, on both
  sides. Each plot has a frontage, a house set back from the street, front and
  back gardens, a fence and a gate. They keep clear of each other, the green,
  the farmsteads, other streets and water.

## Ways

Ways are ranked: highway, road, lane, track, path.

- **Highways** are the consumer's.
- **Roads** join villages by a relative-neighbourhood graph, so the network
  loops rather than branching as a tree. A village near a highway also takes a
  road to it.
- **Lanes** join farmsteads to each other and to villages by the same graph. A
  farmstead hard by a road takes a short lane to it.
- **Tracks** run from a holding's farmstead to its fields' gateways. A holding
  whose farm a village gathers starts its tracks from the nearest greater way
  instead.
- **Paths** take the Gabriel-graph shortcuts the lanes leave out.

Each way is routed over a lattice about its two ends by `lib/terrain`'s integer
A\*, priced by its rank:

- its grade against its rank's;
- wet ground;
- water, by what crossing its rank would build.

Each way follows the greater ways its lattice reaches: their points cost a
third as much, and once laid its line runs on theirs. It keeps clear of
farmstead buildings and gardens. Tracks and paths also keep clear of village
plots.

The search is bounded below by how far each point lies from a greater way, a
chamfer transform of the lattice. That bound never overestimates and never falls
by more than a step costs, so the route found is the cheapest. The search
settles far fewer points than octile distance times the cheapest step would
make it settle. It reads the ground only where it reaches.

The lattice's path is then laid as a line:

1. **Pulled taut.** From each point, the line runs to the farthest point ahead
   whose straight chord costs no more than the path there.
2. **Smoothed.** Corners are cut wherever the cut costs no more than the corner,
   so smoothing never carries a way into a building or water.
3. **Wandered.** The line wanders as its rank does: a path most, a highway not
   at all.
4. **Graded.** Roads and highways are graded to their rank.

## Fields and boundaries

A holding is read on a 4 m raster: water, a bounded way's corridor, a plot, or
land. Its land falls into blocks the ways and water part, and a block too small
to be a field is waste. Each block is cut again and again. Each cut runs across
the frame that the contour sets, or the way beside it, and lies between two
cells' middles. Cutting stops once a piece is the size its ground makes a
field: smaller on good level ground, larger on steep, stony or wet ground. Every
field is therefore the block's land within one convex cell of the holding.

A boundary runs wherever a field meets what is not that field:

- another field, across a cut or a holding's edge (the lesser holding lays the
  edge);
- a bounded way's side;
- a track's end;
- a farmstead's yard.

Each line is read every 2 m for what lies either side of it, and each change is
halved down to a few centimetres. A boundary runs while one pair of sides holds.
Water and village plots bound a field by themselves.

A boundary's kind is drawn by the ground at its middle and the region's
`Style`: walls where stone is at hand, ditches where the ground is wet, fences
near woods, hedges on deep soil. Each holding keeps mostly to its own custom.

Every field is entered by a gateway, chosen in this order:

1. at a track's end that reaches into it;
2. on the best of its yard or the ways it fronts;
3. from the neighbour by which its holding's other fields reach one;
4. failing all of those, across its holding's edges.

A path crosses each boundary it meets by a stile.

## Uses

Each field's use is drawn from the region's `Mix`, weighed by its ground:

- arable on good level ground near the farm;
- pasture on slopes and wet ground;
- orchards and vineyards on warm slopes;
- woodlots and overgrown land on the poorest ground.

A field cut for hay or straw has one kind of bale.

## Laying a region

A `Laying` lays a region out a unit at a time across a `JobRunner`, in this
order:

1. settlements and yards;
2. roads, then lanes;
3. villages' plots;
4. tracks, then paths;
5. fields;
6. boundaries;
7. gates and stiles;
8. uses.

Only as many routes run at once as the runner is wide, so the working memory
is bounded by the runner's width, not the region's size.

Each part is worked out over as much land as its making reads. For example, a
lane is routed wherever a track or path may follow it, and a field's edge reads
the holding beyond. The `Layout` reports every feature reaching its region. A
region laid out in halves therefore lays every feature exactly as the whole
does, and the layout comes out the same on any runner, in any order.

## Testing

The tests lay out a region of rolling hills with a winding river. They check
that:

- the layout is identical on the serial, reversed and threaded runners;
- the halves of a region lay every feature exactly as the whole does;
- no place in a field is under water or in a way's corridor;
- every boundary parts what lies either side of it;
- every field with any frontage is entered;
- no way runs through a building;
- every track ends in its own holding.

Beneath that:

- holdings are shown convex, sharing their edges to the bit;
- villages laid in pieces are the villages laid whole;
- the route guide is checked against an exhaustive search: it never
  overestimates, nor falls by more than a step;
- fields tile their holding's land;
- farm buildings stand apart about their yard.
