# tairix-mix

Stability tier: **experimental**

Integer finalisers, each a bijection with full avalanche — SplitMix64's
output function `mix64` and Wellons's lowbias32 `mix32` — and what is built
on them: the `SplitMix64` generator `mix64` finalises, `numbered` (the key of
a seed's `n`th stream) and `unit` (a word as a fraction of the unit
interval). Every first-party SplitMix64 and lowbias32 in the tree is this
one, so a seed spreads the same way everywhere.
`no_std`, no dependencies, and `#![forbid(unsafe_code)]`.

None is keyed or cryptographic. They spread counters, lattice places and
seeds; a key an attacker chooses wants `lib/hash`'s `SipHash13`, and a secret
wants `lib/crypto`.

## Tests

`cargo test -p tairix-mix`: SplitMix64 against Vigna's published outputs from
seed nought, lowbias32 against a reference implementation written apart from
it, both bijective over a run of counters and flipping half a word's bits for
one flipped, and numbered streams and unit fractions as documented.

The design is in `docs/src/lib/mix.md`.
