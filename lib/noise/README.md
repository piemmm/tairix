# tairix-noise

Stability tier: **experimental**

Procedural noise, every lattice point hashed by `lib/mix`'s lowbias32 rather
than read from a table, so a pattern needs no memory and differs under every
seed. `no_std`, allocation-free, and `#![forbid(unsafe_code)]`; every value is
computed in one fixed order of basic operations through `tairix_util::mathf`,
so it is the same on every target.

## What it provides

- `noise2`, `noise3`: Perlin's improved gradient noise over the plane and in
  three dimensions, nought on every lattice point.
- `fbm2`, `fbm2_resolved`: octave sums over the plane, each octave turned so
  no lattice lines up with the next; resolved, an octave finer than its
  footprint can hold settles to its mean rather than aliasing.
- `ridged2`: ridged octaves branching as eroded crests do.
- `billow2`: billowed octaves, rounded crests and creased troughs, as dunes.
- `ridged2_fading`, `billow2_fading`: either with each octave fainter than the
  last by a gain of the caller's rather than a half, so a relief can lean to
  its longest waves or its shortest.
- `turbulence3`: folded three-dimensional octaves, read as veins.
- `cells2`, `cells3`, `cells3_among`: Worley's cellular noise, the nearest
  two features and the way to the nearest.
- `cell`, `hash2`, `hash3`, `smoothstep`, `resolved`, `octaves_within`: the
  lattice, its hash and the ramps the patterns are made with.

## Tests

`cargo test -p tairix-noise`: the lattice cell is the floor; gradient noise
is nought on the lattice, bounded, smooth, continuous across every lattice
wall and differs under its seed; octave sums stay in range and a resolved sum
keeps only the octaves its footprint holds; cellular noise finds what a
search of the cells about a place finds, a sparse search among kept features
only those; smoothstep runs between its edges.

The design is in `docs/src/lib/noise.md`.
