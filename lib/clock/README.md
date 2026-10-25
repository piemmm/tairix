# tairix-clock

The `clock-v1` controller side (`plans/SOUND.md` SND8, SND19f). Stability
tier: `experimental`.

- `Controller` is the endpoint a clock controller's driver serves under its
  node's duty, written over the `ClockHardware` trait and nothing of any one
  controller: attested clock links, holders tracked per process instance, and
  each clock shared by the policy its hardware states — exact, run at its
  first holder's rate, or floored, run at no less than the highest floor any
  holder states.
- `Node` and `serve` are the freestanding bring-up and loop: the node's grants
  and duty read once, the rate of a fixed clock it is fed from, its register
  window mapped, then the endpoint served through `tairix_drvrt`'s one
  supplier loop.
- `testkit`, behind the `test-support` feature, is the endpoint rig a clock
  driver's own tests drive its hardware through.

Every clock controller driver links it, so the protocol's rules are written
once.
