# TIMEZONES.md — Civil time: zones, the IANA rules, and local rendering

Binding under `AGENTS.md`. This plan gives TAIRiX civil time: the compiled
IANA rule store the system ships, the `no_std` engine that reads an absolute
instant as a local reading and back, the regions whose conventions a reading is
written in, which zone and region a reader is in, and the one renderer every
surface that shows a time draws through.

Nothing here changes what TAIRiX *stores*. Absolute time stays `Time64` UTC
everywhere — syscalls, IPC, logs, file metadata, on-disk formats (`§21`). A zone
is **presentation**, applied when a person reads or types a time, never on the
wire or on disk.

| Item | What it is | Status |
|---|---|---|
| TZ1 | `tools/tzcompile`: the vendored IANA 2026e rules and zone table and the CLDR-derived regions, resolved with `zic`'s semantics; the `zic` oracle | done |
| TZ2 | `lib/tz`: the store reader, zones both ways, regions, places, resolution, `CivilClock`, the fuzz harness | done |
| TZ3 | Every image and the QEMU desktop fixtures plant `/System/Zoneinfo/zones.tzx` | done |
| TZ4 | The machine zone: `system.conf`'s `time.zone`, the desktop's and login screen's clocks reading it; the process-wide `lib/rt` seam lands with TZ7 | in progress |
| TZ5 | The desktop's clocks read local in the user's region, which Settings chooses; the login screen reads the store's default | done |
| TZ6 | `datetime.app` sets the machine zone and composes the instant it commits from local fields | done |
| TZ7 | Every timestamp a command shows reads local, with GNU's UTC switches | planned |
| TZ8 | `README.md`'s matrix and the `docs/src/` pages the stages touch | in progress |

---

## 1. The defect

Without zones every surface shows UTC: `ls`, `stat`, `ps`, `top`, the log
reader, the file manager's properties, the desktop clock and `datetime.app`'s
fields. A person outside UTC reads every time an hour or thirteen wrong, and
then *sets* the clock wrong by "correcting" it — stepping the machine's real
UTC clock off by their offset. There is no way to say "this machine is in
Europe/London", so no surface can be right.

---

## 2. Design

### 2.1 The data is IANA's and Unicode's; the code is ours

Civil-time rules and regional conventions are facts about the world. The
*data* is vendored and the *code* is first-party (`§2.12`): TAIRiX writes its
own compiler and reader and depends on no external crate for either.

- `tools/tzcompile/data/tzdb/`: IANA tzdb 2026e's `tzdata.zi` (built by the
  release's own `make tzdata.zi` recipe, main data form) and `zone.tab`, both
  public domain, from tarballs verified against the release signing key.
- `tools/tzcompile/data/cldr/regions.tsv`: each ISO 3166-1 region's English
  name, numeric date pattern and time-of-day pattern, derived from Unicode
  CLDR 44 (ICU 74.2) by the method its header states, under the Unicode
  License V3.
- Each is pinned by its SHA-256 in the compiler and a mismatch fails the build
  closed (`§19.3`).
- IANA's compiled `TZif` files are not shipped. The ones `zic` 2026e compiles
  from the same `tzdata.zi`, one per canonical zone, are vendored as test data
  only (`tools/tzcompile/tests/reference/`): the oracle every zone is held to.

### 2.2 One compiled store

`tools/tzcompile` is a host library the image build calls; no CLI, since
nothing else runs it. It reads the rules exactly as `zic` 2026e does and
resolves each zone as `zic`'s `outzone`, `stringzone` and `writezone` do, so
what the store holds of a zone is what `zic -b slim` writes: the type before
the first transition, every transition `zic` keeps, and, where `zic` writes a
TZ string alternating two rules, those rules — kept in `zic`'s own rule form,
evaluated with the one rule arithmetic the compiler and reader share
(`tairix_tz::rule`). A string for standard or daylight time all year is the
zone's last type, held for good. The compiler refuses a zone whose rules would
read differently a second after its last transition than at it, and one whose
continuing rule names a day a TZ string cannot state exactly.

The store, `/System/Zoneinfo/zones.tzx`, holds every zone and alias, a shared
type and abbreviation table, the regions, the zone table's places, and the
shipped default zone and region: `Europe/London` and `GB`. Its format is
specified in `docs/src/lib/tz.md` (binding). It is compiled when an image is
built and planted read-only under `/System`, byte-reproducible from the pinned
inputs; it is not committed as a binary.

### 2.3 `lib/tz` — the engine every consumer reads through

- `ZoneInfo::parse` validates a whole store or refuses it with a typed error,
  never panicking (`§2.9`); its bounds are security bounds and fixed
  (`§24.4`). Everything after is allocation-free.
- `Zone::offset_at`: a binary search of the zone's transitions, then its
  trailing rule. `O(log n)`.
- `Zone::local` and `Zone::instant_from_local` over `lib/abi`'s one calendar.
  A local time the clocks skipped or passed twice is a typed error unless the
  caller states which instant it means; nothing is guessed (`§5.4`).
- A zone keeps the name it was found by, so a place listed by an alias reads
  as that place.
- `Region` carries a region's patterns, `Place` a zone-table row, and
  `CivilClock` — a zone read with a region's conventions — is the one renderer
  of a reading's date and time.
- Fuzzed over `parse` and every query of an accepted store, with the
  `local → instant → local` round trip held over generated stores (`§19.6`).

The store is system-shipped, integrity-checked data, so the reader is the
`lib/fdt` class — a total, bounds-checked, fuzzed decoder — not a sandboxed
third-party parser (`§19.5`).

### 2.4 Which zone and region

One resolution function per question in `lib/tz`: the caller's preferences in
order, then the store's shipped default, then — only where no store is planted
— UTC with ISO 8601 dates. It answers which source decided. A name the store
does not hold is refused where it is set, so a mistyped zone is rejected while
the person is looking at it, and passed over where it is read, so a store that
lost a zone degrades to the default rather than to nothing.

The desktop's preferences are two keys of its own settings document,
`region` and `time.zone`, each empty for the store's default; the session
refuses an apply that changes either to a value the store does not hold. TZ4
adds, in order, the `TZ` environment variable — data selecting a presentation,
carrying no authority (`§5.4`), and what GNU tools honour (`§16.7`) — and the
machine setting. No new capability is introduced (`§5.2`): the store is read by
ordinary file access or through a delegation of it (§2.5), and the machine
setting is written under the per-inode gate on `/System/Settings` by the account
elevation the clock already uses.

### 2.5 Reaching the store without filesystem authority

A process that holds no `CAP_FS_ACCESS` reads the store through a read-only
`fd_grant` delegation from the principal it already trusts with its display,
and nothing else crosses: no second format, no paged catalog on the wire, and no
authority beyond reading one public, read-only file.

- **The desktop's Settings application** asks the session
  (`WindowRequest::TakeZoneStore`, answered to Settings alone). The session
  read the store at bring-up through a descriptor it holds for its life, so a
  delegation costs no filesystem operation on its serve loop.
- **The login screen** asks the session authority (`SessionRequest::ZoneStore`,
  answered to the greeter account on its console). The authority opens the
  store, delegates it and closes its descriptor at once, and never parses it:
  no store reader joins the process that adjudicates passwords.

Each reader validates what it read with the one reader, as every other reader
does, and keeps it for its life.

### 2.6 One rendering seam

Every surface that shows a timestamp to a person renders through `CivilClock`.
A program does not re-derive the resolution order or carry its own offset
arithmetic; TZ4 gives non-desktop programs their resolved clock through
`lib/rt`, loaded once.

---

## 3. Invariants

- Stored, compared and transmitted time is UTC `Time64`, unchanged (`§21`).
- One calendar decomposition (`lib/abi`'s `time`), one zone engine
  (`lib/tz`), one rule arithmetic, one store format, one renderer (`§2.2`).
- No zone data is compiled into a binary: the store is a file the image
  builder plants and the engine reads (`§16.5`).
- A missing store reads UTC; a store whose reader refuses it is not read; a
  preference the store lacks is passed over. Nothing fabricates an offset.
- A local time that does not exist, or exists twice, is a typed error the
  caller resolves explicitly.
- Reading an instant allocates nothing and is `O(log n)`.
- Platform-neutral throughout: no `cfg(target_arch)`, no board coupling
  (`§2.20`).

---

## 4. Remaining stages

**TZ3 — shipping the store (done).** The image build compiles the store
through `tools/tzcompile` and plants it read-only on every image and every QEMU
disk that plants the shared bundle set.

**TZ4 — the machine zone.** The machine configuration's `time.zone` key
(`lib/sysconfig`): `default` or a zone name of the one shape `lib/abi`'s `zone`
module defines, so the store, `DesktopInfo` and the setting refuse the same
names. It is written by the settings service and read from the first of the
configuration's layers that exists. Done: the desktop session watches the
configuration's directory, re-reads it on a worker when a change names it,
resolves its clocks by the user's choice, then the machine zone, then the
store's default, and publishes the machine zone in `DesktopInfo` for
applications that may open no file — Settings reads its clocks and labels
*Default* by it; login hands the login screen the machine zone as `TZ`.
Remaining, with TZ7's first command consumer: `lib/rt` resolves `TZ`, then the
machine zone, then the store's default, then UTC, once per process, and hands
out the resolved `CivilClock`.

**TZ5 — the desktop's clocks (done).** The icon-bar clock shows the local time
and the region's numeric date (beneath it where the bar is deep enough, beside
it otherwise), keeping room for the widest each can be; its menu heading adds
the zone's abbreviation; the screensavers tell the bar's own reading; Settings'
Language & Region pane chooses the region and the place and its Date & Time pane
reads local; the login screen reads the store's default zone and region. The
unset placeholder is unchanged (`taskbar::clock::UNSET_LABEL`).

**TZ6 — setting the machine zone (done).** The Date & Time dialog
(`docs/src/desktop/apps.md`) reads and sets the clock in the machine's zone: a
hand-set date and time are composed through `instant_from_local`, a skipped
time refused with the change that skips it, a repeated one asked about by its
two offsets, an implausible one confirmed. Its zone is changed through the place
chooser over `lib/tz`'s `listing` and written as `time.zone` by the settings
service under the account the clock menu authenticates; the zone's next change
is stated (`Zone::next_change`). Settings' Time zone row opens the same chooser
for an account's own zone, *This machine's zone* first. Login exports the
user's own zone as `TZ`, so two people on one machine each read their own local
time (`§26.2`).

**TZ7 — every timestamp a person reads.** `ls`, `stat`, `ps`, `top`, the log
reader, `sysmon`, the text login's header clock and the file manager's
properties render through TZ4's seam
and honour the GNU UTC switches (`§16.7`), each with a regression test at a
fixed zone and instant.

**TZ8 — docs.** `README.md`'s feature matrix and the `docs/src/` pages each
stage touches.

---

## 5. Non-goals

- **A `date` command.** Adding one is its own feature (`plans/APPS.md`); when
  it lands it renders through TZ4 like every other tool.
- **Network time.** Correcting the *instant* is `plans/TIMESYNC.md`'s;
  naming the *zone* is this plan's.
- **Leap seconds.** TAIRiX time is UTC with a linear count (`Time64`); the
  IANA `leapseconds` file is not compiled in and TAI is not modelled.
- **Per-program zone databases.** There is one store and one engine.
