# tairix-mix

`lib/mix` holds the integer finalisers the tree spreads its counters,
lattice places and seeds with, each defined once:

- **`mix64`** is SplitMix64's output function (Steele, Lea and Flood, "Fast
  Splittable Pseudorandom Number Generators", 2014) and **`SplitMix64`** the
  generator it finalises: a Weyl sequence stepped by `GOLDEN_GAMMA`, each
  step's state finalised. `lib/rng` seeds its generators from it, `lib/abi`
  draws a load slot with it, and the host tests' seeded draws are it.
- **`mix32`** is Wellons's lowbias32, the 32-bit finaliser procedural noise
  hashes its lattice by (`docs/src/lib/noise.md`).
- **`numbered(seed, index)`** keys the `index`th of the streams drawn from
  one seed, so streams numbered alike under different seeds, or under one
  seed differently, share nothing.
- **`unit(bits)`** is a 32-bit word as a fraction of `0.0..1.0`.

Each finaliser is a bijection with full avalanche: a flipped input bit flips
about half the output's. None is keyed or cryptographic, so an input chosen to
collide collides; a hash over keys an attacker chooses is `lib/hash`'s
`SipHash13`, and a secret is `lib/crypto`'s. The crate has no dependencies and
is `no_std`, so the lowest crate in the tree can take it.
