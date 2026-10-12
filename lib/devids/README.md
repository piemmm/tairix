# tairix-devids

Hardware-identity database engine for TAIRiX (`lib/devids`). Stability tier:
**experimental**.

The `lspci` and `lsusb` command apps (plans/DEVICES.md DEVICE1) name the
devices the hardware tree discovers, and the network map (plans/NETMAP.md
NM-F) names the organisation behind a hardware address. The names come from
the public PCI (`pci.ids`, pci-ids.ucw.cz) and USB (`usb.ids`, linux-usb.org)
ID databases and the IEEE MA-L registry (`oui.ids`, standards-oui.ieee.org),
imported as vetted, provenance-pinned snapshots and compiled into compact
binary lookup tables. This crate is the one definition of that pipeline —
grammar, vetting filter, table format, encoder, decoder and loader — shared by
the `cargo xtask devids` generator, the CI drift gate, and the runtime
consumers.

## Pipeline

1. `cargo xtask devids --fetch [<database>]` (developer-run only, never CI or
   the build) downloads every database, or the one named, runs the vetting
   filter, and rewrites the committed snapshots under `lib/devids/assets/`
   with a provenance header (upstream URL, version and date where upstream
   publishes them, fetch date, SHA-256 of the raw download, licence statement,
   and for the IEEE registry what its conversion changed). The IEEE publishes
   CSV with postal addresses; the import keeps each assignment and its
   organisation's name, respaced and stripped of characters that hide or
   reorder text, and omits an assignment listed for several organisations or
   held by the registry to sub-allocate, since none names the device's maker.
   The refresh diff is human-reviewed like any other change.
2. `cargo xtask devids --write` regenerates the compact tables from the
   committed snapshots. Each table is written into its consuming bundle's
   `Resources/` directory as `<name>.bin`
   (`userland/apps/lspci/Resources/pci.ids.bin`,
   `userland/apps/lsusb/Resources/usb.ids.bin`,
   `userland/apps/netmap/Resources/oui.ids.bin`), so it ships inside the
   self-contained bundle with no second copy in the tree. The generator refuses
   to emit a table over `MAX_TABLE_BYTES`, the bound its consumer reads.
3. `cargo xtask devids` (no flag; part of `cargo xtask ci`) re-runs the
   converter and fails closed on any drift between snapshot and tables.

The generated `.bin` tables are data files a bundle ships as a resource; they
are never `include_bytes!`d into a binary.

## Vetting

The raw download is untrusted input whose strings end up on users'
terminals and desktops, so `textdb::parse` is strict and fail-closed: the
whole file must match the shared line grammar (never skip-and-continue), names
must be valid UTF-8 with no character that hides or reorders text
(`tairix_util::utf8::is_hidden`: no control, so no terminal escape injection,
and no bidirectional control or zero-width character, so no name passes for
another or hides text from a refresh's reviewer), ids are exact-width lowercase
hex in every emitted scope, duplicates within a scope reject the import, and
source size, name length, and entry counts are bounded (fixed security bounds
with generous headroom — the largest table, the MA-L organisations, is
~40 000 records against a 262 144 bound).

PCI subsystem entries and the auxiliary `usb.ids` sections (`AT`, `HID`,
`R`, `BIAS`, `PHY`, `HUT`, `L`, `HCC`, `VT`) are validated but not encoded:
no consumer renders them today (the hardware tree records no subsystem
ids), and an unused table would be speculative surface.

## Lookup

`DevIds::parse` validates a compiled table fail-closed (magic, kind, exact
length, sorted keys, every name slice in-bounds on character boundaries) once,
over bytes it borrows (`DevIds<&[u8]>`) or owns (`DevIds<Vec<u8>>`), so a
long-lived owner never revalidates. Lookups are allocation-free O(log n)
binary searches: `vendor`, `device`, `class`, `subclass`, `prog_if`, and
`organisation` for an OUI table. An id the database does not name is `None`;
the caller renders the numeric form rather than fabricating a name. A refused
table's `TableError` says what is wrong in a sentence.

## Loading

`bundled::load(kind, word)` (feature `rt`) reads the program `word`'s own
table out of its system bundle's `Resources/`, found through the one shared
bundle-resolution policy with an empty environment — the order a program's own
help is found by — so a user-writable directory cannot answer for a system
program's table. The read is bounded by `MAX_TABLE_BYTES`; a `LoadError` says
why it failed, and the caller goes on without names.

## Features

- `textdb` (default): the snapshot parser/vetting filter/encoder (needs
  `alloc`). A lookup-only consumer builds with `default-features = false`.
- `rt`: the bundled-table loader, enabled only by a freestanding `Run`
  binary's target-gated dependency.

## Testing

Unit tests cover the accept/reject matrix of the vetting filter, encoder
determinism and interning, decoder corruption handling, owned and borrowed
tables answering alike, every refusal's message, and the committed snapshots
(including well-known organisations). `tests/fuzz_devids.rs` is the in-tree
fuzz harness (`cargo xtask fuzz`), driving both the text parser and the table
decoder with seeded hostile inputs.
