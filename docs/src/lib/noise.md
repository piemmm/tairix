# tairix-noise

`lib/noise` is the procedural noise the land generator (`docs/src/lib/land.md`)
and the ray tracer (`docs/src/lib/raytrace.md`) draw their patterns from. It
is `no_std`, allocation-free and forbids `unsafe`.

- **Hashed, not tabled.** A lattice point's gradient or feature is picked by
  hashing its coordinates under the pattern's seed with `lib/mix`'s
  lowbias32, so a pattern holds no permutation table, every seed gives a
  different one, and a wall between two cells hashes the same from either.
- **Gradient noise.** `noise2` and `noise3` are Perlin's improved gradient
  noise: nought on every lattice point, its quintic fade smooth to the second
  derivative across every wall. The plane's eight gradients are equal in
  length; its result is scaled so it spans about `-1..=1`.
- **Octaves.** `fbm2` sums octaves of plane noise, each turned by a fixed
  irrational angle, `atan(3/4)`, once more than the last, the turns
  repeating every eight octaves, so no lattice lines up with another within
  a factor of 256; `fbm2_resolved` keeps of each octave only what a
  footprint of its own units can hold, the rest settling to its mean, so a
  pattern sampled far off neither aliases nor changes its scale. `ridged2`
  folds each octave at nought and weights it by the crest beneath, so ridges
  branch as eroded mountains do; `billow2` folds each octave at nought
  without the weighting, so its crests round and its troughs crease, as sand
  lies in dunes. `turbulence3` folds three-dimensional octaves into veins.
- **Cellular noise.** `cells2` and `cells3` are Worley's: one jittered
  feature to a unit cell, the nearest two among the cells about a place and
  the way to the nearest, each feature's place drawn apart from the id it
  answers with. The search holds the true nearest wherever no feature strays
  past half way to its cell's edge. `cells3_among` finds the nearest two
  among a sparse set of features, and skips every cell whose feature cannot
  lie nearer than the second found.
- **Lattice hashes.** `hash2` is `hash3` over the layer `y = 0`, and
  `noise3`'s gradients there are `noise2`'s, so a plane field and a space's
  drawn under one seed share their draws there: they are kept apart by
  their seeds.
- **The same everywhere.** The cell of a coordinate is its floor by
  truncation, with no rounding routine that stands in for an instruction on
  the baseline x86-64 target, and every value is computed in one fixed order
  through `tairix_util::mathf`, so a pattern is bit-identical on every Tier-1
  target.
