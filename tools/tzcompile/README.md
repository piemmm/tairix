# tairix-tzcompile

The host-only compiler of the civil-time zone store,
`/System/Zoneinfo/zones.tzx` (`plans/TIMEZONES.md`). The image builder plants
what `compile` returns; nothing commits the store itself.

Stability tier: **experimental** (host build tooling).

## Inputs

Each vendored input is pinned by its SHA-256 in `src/lib.rs`, and a mismatch
fails the build closed.

| File | Source | Licence |
|---|---|---|
| `data/tzdb/tzdata.zi` | IANA tzdb 2026e, built by its own `make tzdata.zi` recipe (`ziguard.awk` then `zishrink.awk`, main data form, no packrat data) | public domain (`data/tzdb/LICENSE`) |
| `data/tzdb/zone.tab` | IANA tzdb 2026e, as released | public domain |
| `data/cldr/regions.tsv` | derived from Unicode CLDR 44 as ICU 74.2 ships it; the derivation is stated in the file's header | Unicode License V3 (`data/cldr/LICENSE`) |

The tzdb release tarballs were verified against Paul Eggert's signing key
(`7E3792A9D8ACF7D633BC1588ED97E90E62AA7E34`) before the files were taken.

The shipped default region and zone, `GB` and `Europe/London`, are
`DEFAULT_REGION` and `DEFAULT_ZONE` in `src/lib.rs`.

## Semantics

`source` reads the rules exactly as `zic` 2026e does — its field splitting,
its keyword abbreviations, its `gethms` rounding, its year and day grammar —
and `outzone` resolves each zone as `zic`'s `outzone`, `stringzone` and
`writezone` do, without the options TAIRiX never passes (no leap seconds, no
`-r`, slim output). What the store holds of each zone is therefore what
`zic -b slim` writes: the type before the first transition, every transition
`zic` keeps, and, where `zic` writes a TZ string alternating two rules, those
two rules as the zone's trailing rule; a string for standard or daylight time
all year is the zone's last type, which holds for good. `compile` also refuses a zone whose trailing rule
reads differently a second after its last transition than at it, and one
whose trailing rule names a day the TZ string cannot state — a weekday on or
after the 29th, which `zic` writes as the month's last, or on or before the
6th — rather than read it otherwise than `zic`'s output does.

## Tests

- `tests/oracle.rs` holds every canonical zone to the `TZif` file `zic` 2026e
  compiled from the same `tzdata.zi` (`tests/reference/`, public domain):
  each file is read by its RFC 9636 layout and its TZ string by POSIX's
  rules, and the two readings are compared just before and at every change
  either makes from 1800 to 2500, and at fixed points beyond.
- `tests/zones.rs` states what the hardest zones read: local mean time,
  double summer time, negative saving, three-quarter-hour offsets, half an
  hour of saving, a southern summer, a zone that changed its rules twice.
- The unit tests pin the source grammar, including the fractional-second
  rounding `zic` actually does.

## Updating to a new release

1. Verify the new tzdata and tzcode tarballs' signatures.
2. Build `tzdata.zi` with the release's own `make tzdata.zi` and replace
   `data/tzdb/tzdata.zi` and `data/tzdb/zone.tab`.
3. Build that release's `zic` and regenerate `tests/reference/` with
   `zic -b slim` from the new `tzdata.zi`, one file per canonical zone.
4. Where the release changes `iso3166.tab`, or CLDR is moved on, derive
   `data/cldr/regions.tsv` again by the method its header states.
5. Update the pins in `src/lib.rs`; the release, zone and link counts and the
   reference manifest's hash `tests/oracle.rs` asserts; and the region and
   place counts `tests/zones.rs` asserts.
