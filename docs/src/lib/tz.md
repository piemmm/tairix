# `lib/tz` — civil time zones

`tairix-tz` reads the compiled zone store, `/System/Zoneinfo/zones.tzx`, and
answers everything a surface needs to show a person the local time. Stored and
transmitted time stays UTC `Time64`; a zone is presentation, applied when a
person reads or types a time. The design is `plans/TIMEZONES.md`; the store is
compiled by `tools/tzcompile`.

The rules and the zone table are IANA tzdb's, in the public domain. The
regions' names and patterns are derived from Unicode CLDR, Copyright © 1991-2026
Unicode, Inc., under the Unicode License V3
(`tools/tzcompile/data/cldr/LICENSE`).

`ZoneInfo::parse` validates a whole store or refuses it with a `StoreError`;
nothing of a refused store is read. Every query after that is allocation-free
and meets nothing it must refuse, so a reader holding the store keeps it as a
borrowed byte slice and nothing more.

## Zones

- `ZoneInfo::zone(name)` finds a zone by its canonical name or an alias, by
  binary search, and the `Zone` keeps the name it was found by: Ghana's place
  is `Africa/Accra`, an alias of Abidjan's rules, and reads as Accra.
- `Zone::offset_at` is the offset, abbreviation and daylight flag in force at
  an instant: a binary search of the zone's transitions, then its trailing
  rule past the last one.
- `Zone::local` reads an instant as local civil fields.
- `Zone::next_minute` is the instant a clock showing the zone's minutes next
  changes: its next local minute, or its next change of offset where that
  comes first. A zone whose offset is not a whole number of minutes turns its
  minutes on its own seconds.
- `Zone::instant_from_local` goes the other way. A local time the clocks
  skipped is `LocalError::Gap`; one they passed twice is
  `LocalError::Overlap` unless the caller says which instant it means.
  Nothing is guessed.
- `Zone::UTC` stands where no store does.

## Regions, places and the clock

A `Region` is an ISO 3166-1 region with its English name and its numeric date
and time-of-day patterns. A pattern is the subset of Unicode CLDR's notation a
clock shows: `d` or `dd`, `M` or `MM`, and `y` or `yyyy` once each for a date;
`H` or `HH`, or `h` or `hh` with `a`, and `mm` for a time; and between them
only ASCII punctuation and spaces. `y` is the whole year and `yyyy` the year in
at least four digits after its sign, as ISO 8601 writes it. A `Place` is a row
of IANA's zone table: a zone by the name the table lists it under, its region,
and the comment that tells a region's zones apart. `Place::write_city` writes
the city its name ends with.

`CivilClock` is a zone read with a region's conventions, and the one renderer
of a reading's date and time every clock draws through. With no region it
writes ISO 8601's `yyyy-MM-dd` and a 24-hour clock. `WIDEST` are the local
times at which every field a pattern holds is at its widest, before noon and
after, so a display that keeps room for what they write never moves.

`resolve_zone` and `resolve_region` take the reader's preferences in order and
answer the first the store holds, else the store's default, else UTC with no
region, saying which decided (`Decided`). A preference the store does not hold
is passed over, never guessed at.

## Reading the store

With the `rt` feature, `load` reads the store through a descriptor the caller
holds, validates it, and keeps it for the life of the process, since every
zone and clock borrows it. The descriptor is one the caller opened at
`STORE_PATH`, or — for a process holding no `CAP_FS_ACCESS` — a read-only
delegation from the principal it trusts with its display: the desktop session
for Settings, the login authority for the login screen
(`plans/TIMEZONES.md` §2.5). Two stores are equal when they hold the same bytes.

## The store format

The compiler writes the format and this crate reads it; both build through the
one definition in `layout`, and the writer (`write`, behind the `write`
feature) hands its output to the reader before returning it. Every integer is
little-endian and read with `from_le_bytes` over a slice, so a store needs no
alignment and reads alike on every target. String offsets count from the start
of the string section.

The header, 60 bytes:

| at | type | field |
|---|---|---|
| 0 | `[u8; 4]` | magic, `TZX1` |
| 4 | `u16` | format version, `1` |
| 6 | `u16` | flags, `0` |
| 8 | `u32` | offset of the IANA release, such as `2026e` |
| 12 | `u32` | zones |
| 16 | `u32` | names, canonical and alias |
| 20 | `u32` | transitions, all zones |
| 24 | `u32` | types |
| 28 | `u32` | abbreviations |
| 32 | `u32` | trailing rules |
| 36 | `u32` | regions |
| 40 | `u32` | places |
| 44 | `u32` | bytes in the string section |
| 48 | `u32` | index of the name of the zone a reader with no preference is in |
| 52 | `[u8; 2]` | code of the region a reader with no preference is in |
| 54 | `u16` | `0` |
| 56 | `u32` | CRC-32C of every byte of the store but these four |

Then nine sections in this order, each packed:

1. **Names**, 8 bytes each: name offset (`u32`), zone index (`u32`).
   Strictly ascending by the names' bytes; a canonical zone's own name points
   at it, and an alias at the zone it names.
2. **Zones**, 16 bytes each: canonical name offset, first transition, number
   of transitions, trailing rule index or `u32::MAX` for none (`u32` each).
   Zones are in name order and their transitions follow one another.
3. **Transitions**, 10 bytes each: the UT second a type begins (`i64`), the
   type (`u16`). Strictly ascending within a zone, the first at `i64::MIN`.
4. **Types**, 8 bytes each: offset east of UT in seconds (`i32`),
   abbreviation index (`u16`), daylight flag (`u8`, `0` or `1`), `0` (`u8`).
5. **Abbreviations**, 4 bytes each: string offset (`u32`).
6. **Trailing rules**, 36 bytes each: the standard offset (`i32`), then the
   change into daylight saving and the change back to standard time, 16 bytes
   each: saving (`i32`), abbreviation index (`u16`), month `1..=12` (`u8`),
   day code (`u8`: `0` the day itself, `1` the weekday on or after it, `2` the
   weekday on or before it), day (`u8`), weekday (`u8`, `0` Sunday, `0` for
   day code `0`), clock (`u8`: `0` wall, `1` standard, `2` universal), `0`
   (`u8`), seconds after that day's midnight (`i32`).
7. **Regions**, 16 bytes each: ISO 3166-1 alpha-2 code (`[u8; 2]`), `0`
   (`u16`), name, date pattern and time pattern offsets (`u32` each).
   Strictly ascending by code.
8. **Places**, 12 bytes each: name index (`u32`), region code (`[u8; 2]`),
   `0` (`u16`), comment offset (`u32`). In the zone table's order.
9. **Strings**: NUL-terminated UTF-8.

A trailing rule continues its zone strictly after the zone's last transition:
at each instant the latest change either half makes decides, the offset being
the standard offset plus that half's saving. Each half's change in a year is
the day it names in that year at its time of day, read on its clock — the wall
clock with the other half's saving in force, standard time, or UT.

A store is refused whole for any of: a short or foreign header; a version, flag
or reserved byte the format does not define; sections that do not exactly fill
it; a checksum that does not match; a string that is unterminated, not UTF-8,
or of the wrong shape; an index or offset outside what it indexes; a table out
of order or naming something twice; a zone whose first transition is not
`i64::MIN`; an offset, a saving, or a trailing rule's standard offset plus
either saving more than 26 hours, or a rule time more than a week, from zero; a
rule naming a day some year lacks (the 29th of February only as an on-or-before
bound); a region name or place comment holding a character that hides or
reorders text; a pattern outside the subset above; or a default zone or region
the store lacks.

Its bounds are security bounds and fixed: the store at most 4 MiB; a zone name
at most 64 bytes of ASCII letters, digits, `_`, `-` and `+` in `/`-joined
components; an abbreviation 1 to 16 bytes of ASCII letters, digits, `+` and
`-`; a region name at most 96 bytes; a pattern at most 16; a place comment at
most 128; the release 1 to 16 ASCII letters and digits.
