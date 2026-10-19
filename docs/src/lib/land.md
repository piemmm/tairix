# tairix-land

`lib/land` is the land generator the desktop's ray tracer
(`docs/src/lib/raytrace.md`) builds its landscapes with and WinterSun's world
builds its realm's land with (`plans/RAYTRACE.md` RT40, `plans/WINTERSUN.md`
WS2). It is `no_std` + `alloc` and forbids `unsafe`.

- **Relief** (`relief`). A landform is a height function of place alone:
  rolling hills, ridged mountains about a valley floor, dunes across the
  wind, an island, terraced mesas, a river valley whose sides climb at a
  slope soil keeps to. A land lays one out over a disc on a regional slope,
  levelled in a clearing whose edge wanders, and either runs on past the disc
  or settles toward its edge to a rim.
- **Courses** (`course`). Rivers, roads and paths are polylines whose marks
  carry the level the course runs at, its breadth and depth, and along a
  river how far its water has run, its units of pool and riffle, its bend and
  its fall. An index buckets every segment into the squares it can reach, so
  the nearest course to a place is found in one square's worth of segments.
- **Channels** (`channel`). A river's channel is drawn from where a place lies
  along its course and the land's seed alone: units of pool and riffle five
  to seven widths apart, ledges where bedded rock holds a reach, the deepest
  line swinging from bank to bank, bars on the inside, cut banks on the
  outside, its water ponded behind each crest at low flow.
- **Snow** (`snow`). A fall is moved before it settles: lifted where the
  ground stands exposed, dropped in the lee of every rise as the ground
  upwind shelters it, quickened over crests, drifted against walls and hedges
  as their porosity has it, and carved where the wind keeps working into
  dunes and sastrugi.
- **What grows** (`grown`). A farmed land's fields grow by season — each
  cereal's stage, maize, rape, ley, hay cut or standing, orchard, vineyard,
  woodlot, left to grow over — and a field's rows run along its length; each
  is a byte a vertex carries, read at the nearest vertex and never blended.
- **What a vertex carries** (`attributes`). Six quantities blended between
  vertices — wet, sediment, road, path, green, snow — and the two categories
  of what grows and the way its rows run.

## The coarse land

A land is planned whole (`plan::Plan`) and built a bounded unit at a time
(`coarse::Build`). Its coarse lattice holds the plan's fixed sample count
over the land's breadth, its square snapped to whole steps of a lattice fixed
to the land (`Plan::lattice`), and the wear runs over all of it at once:
Priority-Flood drainage, implicit stream-power incision along it and hillslope
creep and slumping, pass after pass. The last drainage says where the rivers
run and the lakes stand and how wet each sample lies; the road is routed and
graded, bridging its rivers; the countryside is laid out over the land and its
roads, lanes, tracks and paths laid in, with the ditches dug along its
boundaries and the gateways its stock churn; and where snow lies, how
sheltered from the wind each sample stands is read. The finished `Coarse` is
everything a finer level reads. A consumer may join courses of its own to it
before refining its finer levels (`Coarse::join`): a footpath from where one
stands, the level beds a work cuts into a hillside. A land raised from a
relief runs on past its square, and `Coarse::beyond` reads it there, out to
the horizon.

A coarse land may instead be brought from outside (`coarse::Field`,
`Coarse::of_field`): a consumer that solved its own — WinterSun's realm field —
hands over its lattices, the heights at its samples, the water surface its
drainage filled, how wet each sample lies, the grain of its relief at each and
its character. The lakes stand where the filled surface lies deep over the
ground, and the levels refine it exactly as they refine the crate's own; such a
land ends at its square.

What the levels read stands apart from the plan: their geometry
(`plan::Lattices` — steps, domains, halos, the squares a piece holds and
reads, what refining one costs), the relief's character below the coarse step
and what lies over the land (`plan::Character`), its grain at each place — how
rough it is, how much of it runs in ridges, dunes or gullies rather than
rounded hummocks, which of its waves it leans to (rolling swells, or ground
breaking at its finest waves), and the knobs of a few metres it is studded
with, each level adding what it resolves of them beyond its parent
(`plan::Grain`) — even
or read between the coarse samples (`plan::Grains`), the clearing
(`relief::Clearing`), the rivers' form and the season.

## Levels and pieces

Each level lies on a lattice fixed to the land, its step its parent's over a
whole ratio (`plan::Level`), level 0's parent the coarse lattice. A piece is a
square of a level's lattice (`level::Refining`, `level::Piece`), and a pure
function of the land and the square:

- **Its ground** before the droplets is its parent read through a Catmull–Rom
  patch — the worn coarse land for level 0, the level before it after that —
  the level's band of detail keyed by place, carrying on down the spectrum
  where its parent left off and of the land's grain there, the beds the land's
  courses cut, the sea floor shelving away under its shore's face, and the snow
  as the level lies it — on every level after the first drifted against what
  stands about it as finely as the level resolves, and carved by the wind.
- **Its droplets** run on the level's lattice over the land's square, the
  piece worn from blocks of ground a consumer's store keeps where it keeps
  them, the rest filled within the run's halo (`Lattices::held`) and worn by
  just the tiles they need, so it wears exactly as the level worn whole does
  (`docs/src/lib/terrain.md`). A finer piece reads its parent over everything
  its held square's patches reach (`Lattices::needs`); a parent that holds less
  is refused rather than read short, and one keeping only its heights reads as
  plain ground.
- **What it keeps.** A consumer reading only heights refines them alone
  (`level::Keeps::Heights`): its vertices carry nothing and no water is
  tracked, and its heights are the same to the bit. A level takes its parent's
  snow out of the ground beneath it, so where snow lies a parent keeping only
  its heights is refused.
- **Its settling** puts its rivers' channels and its ways back to the beds they
  were cut to, holds its clearing and its cuttings to their levels, and on level
  0 takes into each vertex the wet and the silt the droplets left.

A region refined whole and the same region refined in pieces are one land, to
the bit: the land is the same wherever a consumer stands, and whoever reads it.
What a piece costs is known before it is refined (`Lattices::cost`), so a consumer
weighs its work before doing it.

Everything is a pure function of its arguments in one fixed order of basic
operations through `tairix_util::mathf`, so a land is bit-identical on every
Tier-1 target, whichever consumer reads it and wherever it is read from.
Every stage takes its allocations fallibly and refuses a plan or a piece
that does not fit (`Error`), never panicking.
