# tairix-datetime — the Date & Time dialog (`datetime.app`)

Stability: **experimental**

The machine's Date & Time dialog (`plans/TIMEZONES.md` TZ6): the clock to the
second and where its time came from, network time on or off, the date and time
set by hand in the machine's zone, and the zone itself. The design is
`docs/src/desktop/apps.md` (*Date & Time*).

## What is where

- **`src/lib.rs` — the engine.** The readout and its provenance, the zone's
  next change, a hand-set instant composed through `Zone::instant_from_local`
  (skipped, repeated and implausible times), and the words every outcome is
  stated in. `no_std` + `alloc`, host-tested.
- **`src/calendar.rs`, `src/time_entry.rs` — the two controls only this
  dialog has.** A calendar month on the region's first weekday, and hours,
  minutes and seconds (and AM/PM on a twelve-hour clock) composed from the
  shared number field and button.
- **`src/view.rs` — the window.** A dialog window's client sized to its
  content: its layout, its paint, the keyboard's walk and the routing of every
  input, one layout for all three.
- **`src/run.rs` — the `Run` binary.** The window and its owned zone chooser,
  one event mailbox, a tick on the clock's own second, and one worker on a
  queued desk for every read and write. An inert stub on the host.

## The authority it is given

`CAP_TIME_SET` steps the clock and sets the clock chip; `CAP_SETTINGS_WRITE`
asks the settings service to turn network time on or off and to change the
machine's zone; `CAP_FS_ACCESS` reads the zone store and the time service's
record; `CAP_SHM` and `CAP_CONSOLE_WRITE` serve the window and its
diagnostics. The kernel grants `manifest ∩ the launching account's ceiling`,
so a refused change is an answer the dialog states, never a failure it hides.

## Tests

`cargo test -p tairix-datetime` covers the engine, both controls, the
chooser's rows and the window: its layout under every theme, ladder and scale,
the keyboard's walk, network time taking the hand-set controls away, a chosen
day and a typed time made the user's own and set, and the damage a tick
reports. The freestanding `Run` body is linted with
`cargo clippy -p tairix-datetime --target aarch64-unknown-none -- -D warnings`.
