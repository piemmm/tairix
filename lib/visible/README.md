# tairix-visible

Stability tier: **experimental**

The one rule for the characters that draw nothing where they stand yet can
hide, reorder or break the text around them, and text shown with each of them
replaced by U+FFFD, so what is drawn is what is there.

## What it provides

- `is_hidden` — whether a character is one of them: a C0 or C1 control, a line
  or paragraph separator, an interlinear-annotation control, or a
  default-ignorable code point (Unicode `DerivedCoreProperties.txt`) — the
  bidirectional controls, the zero-width characters, the fillers, the
  variation selectors and the tags.
- `shown_char` — one character as it is shown: U+FFFD for a hidden one.
- `Shown` — any `Display` formatted with each hidden character replaced,
  straight into its destination, without allocating.

## Why it is a crate

The rule is held by the terminal vocabulary (`lib/vt`'s emitter prints a hidden
character as U+FFFD), the curses cells, the desktop's names and alerts, the
font service and the name decoders alike, so it has one definition. `lib/vt` is
linked by every architecture port's boot console, and a QEMU vertical links a
port with no allocator at all, so the rule's home must link none under any
combination of features a build unifies — which `lib/util`, whose members hold
text on the heap, cannot be. `no_std`, no dependencies, and no `unsafe`.

`lib/util`'s `utf8::shown` is the allocating form, a name answered as a
borrowed or owned string.
