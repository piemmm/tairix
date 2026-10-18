# tairix-tz

Stability: **experimental**.

Civil time for TAIRiX: the reader of the compiled zone store
`/System/Zoneinfo/zones.tzx` and everything a surface needs to show a human the
local time.

- `ZoneInfo::parse` validates a whole store — its bounds, ordering, checksum
  and every string — or refuses it, allocation-free and without panicking.
- `Zone` answers the offset in force at an instant, the instant its clock's
  minute next turns, and converts both ways between an instant and local
  civil fields; a local time that does not exist, or exists twice, is a typed
  error unless the caller says which it means.
- `Region` carries a region's numeric date and time-of-day patterns, and
  `Place` a zone the IANA table lists for a region.
- `CivilClock` is the one renderer of a reading's date and time.
- With the `rt` feature, `load` reads a store whole through a descriptor the
  caller holds — one it opened, or one delegated to a process that may open no
  file — validates it, and keeps it for the life of the process.
- `layout` and `rule` are the store's wire layout and the zone-rule
  arithmetic, shared with `tools/tzcompile`, which writes the store.

The format is specified in `docs/src/lib/tz.md`; the design is
`plans/TIMEZONES.md`.
