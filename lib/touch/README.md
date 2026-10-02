# tairix-touch

The seat's touch gesture recogniser (`plans/POINTING.md`): it reads the touch
frames a seat owner drains from its touch channel and answers pointer motion,
clicks, scrolls and pinches. What a contact means is seat policy, so it is
decided here once, for the desktop session and the greeter alike.

- `Recogniser` follows each surface apart, keyed by the injector the kernel
  stamped on its frames and the injector's own device index. It allocates
  nothing; between frames it needs waking only at the instant
  `deadline_ns` states, and every queued frame is fed before `expire` runs.
- `Gesture` is what it answers: `MovedBy` (a touchpad's pixels at the user's
  speed), `MovedTo` (a touchscreen's place), `Pressed`/`Released`
  (`TouchPress::Device` for a surface's own buttons, which the seat maps
  through its button order; `TouchPress::Fingers` for a click the fingers
  made), `Scrolled` (scroll units, both axes) and `Pinch` (phase, scale in
  16.16 relative to its start, and on a touchscreen its centre).
- `TouchSettings`: tap to click, natural scrolling, and the touchpad speed.

Touchpad: one finger moves the pointer with a gain that rises with finger
speed; motion waits while the touch could still be a tap, so a tap clicks
where the pointer was. A one-, two- or three-finger tap clicks the primary,
secondary or middle button; a one-finger tap's press is held for a moment so a
touch that follows drags, and a second tap makes a double click. Two fingers
scroll — keeping to the axis they began on unless they began diagonally — or
pinch, whichever their centre's travel or their spread's change shows first. A
clickpad's press is the button its fingers count; while any button is held the
finger that moves drives the pointer and no gesture begins.

Touchscreen: one finger puts the pointer where it lands and presses once it
moves past a small slop or rests, so a second finger landing with it begins a
two-finger scroll or pinch at their centre instead of clicking. A finger left
after a gesture does nothing until every finger lifts.

A contact the device judges a palm takes part in nothing; a contact a frame no
longer names has lifted. The recogniser follows at most eight surfaces at once
and lets the least recently fed go — releasing what it held and cancelling its
pinch — when another arrives: a bound on what injectors can make the seat
hold, not a capacity.

## Testing

Scenario tests in `src/tests.rs` pin each behaviour on a surface whose
normalised step is exactly a tenth of a millimetre, so every expected distance
is exact. `tests/fuzz_touch.rs` (a `cargo xtask fuzz` target) feeds arbitrary
valid frame sequences — surfaces of every kind and size, lost and palm
contacts, buttons, out-of-order and extreme times, resets — and holds the
recogniser to releasing only what it pressed, one pinch life per surface,
deadlines always ahead of the last expiry, and no overflow.

## Stability

Tier: `experimental`.
