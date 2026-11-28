# tairix-riscos-session

TAIRiX Bridge's session (`plans/RISCOS-EMULATOR.md` §15): the protocol between
the front end and a session's worker, and the two sides that speak it. The
description is `docs/src/desktop/tairix-bridge.md`.

Its notices are [the bridge's](../../README.md).

Stability: `experimental`.

- `protocol` — the commands the front end sends (the session's start, run
  and hold, credited input, the damage ring's space released, relays
  answered, the boot, launches, quits and pauses, the pressure band and the
  desktop font) and the events the worker sends (input taken in, launches
  made or refused, screens, their damage, cursors, pointers and hourglasses,
  memory held,
  relays to carry, the boot's failures, contexts ended), each decoded whole
  against fixed bounds.
- `ring` — the screens' damage ring in the spawn share, placed and released
  alike on both sides, so the front end believes a record only where the
  worker could have put it.
- `credit` — each context's credited input: the front end's window and the
  worker's inbox.
- `worker` — the worker: the kernel and every context of the session,
  scheduled in turns, paced to the port's clock, its screens shown through the
  ring, its relays carried, and a front end that breaks the protocol ending
  it.
- `front` — the front end's model of the session: an event believed only
  where the worker could have sent it, and a session that ended reported with
  the contexts that were running.
- `host` — the front end's side of one worker, driven without blocking:
  commands queued in order, events believed, damage copied out of the ring,
  input held past a context's credit.

It holds no `unsafe` code. `tests/fuzz_protocol.rs` is its fuzz harness and
`tests/worker.rs` runs the worker over the real kernel, filing system and
BASIC.
