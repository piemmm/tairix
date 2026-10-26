# tairix-visible

Stability tier: **experimental**

The one rule for showing text from elsewhere so that what is drawn is what is
there: each character that draws nothing where it stands yet can hide, reorder
or break the text around it replaced by U+FFFD, and a combining sequence cut at
one U+FFFD where it would carry more than four marks or one twice (UTS #39
§5.4), so two names that differ never look alike.

## What it provides

- `is_hidden` — whether a character is one of them: a C0 or C1 control, a line
  or paragraph separator, an interlinear-annotation control, or a
  default-ignorable code point — the bidirectional controls, the zero-width
  characters, the fillers, the variation selectors and the tags. Read from
  `lib/unicode`'s tables, generated from the pinned Unicode Character Database,
  never a hand-kept list.
- `shown_char` — one character shown on its own, as a cell or a glyph is:
  U+FFFD for a hidden one.
- `Shown` — any `Display` formatted as text from elsewhere is shown, straight
  into its destination, without allocating; `shown_chars` — the same
  characters, lazily, for a surface laying them out under its own budget.
- `Faithful` — the lossless twin, for the renderings that escape rather than
  replace (the log renderer, GNU quoting, DNS display labels): which characters
  such a rendering escapes.

## Why it is a crate

The rule is held by the terminal vocabulary (`lib/vt`'s emitter prints a hidden
character as U+FFFD), the curses cells, the desktop's names, titles and alerts,
the font service and the name decoders alike, so it has one definition. `lib/vt`
is linked by every architecture port's boot console, and a QEMU vertical links a
port with no allocator at all, so the rule's home must link none under any
combination of features a build unifies — which `lib/util`, whose members hold
text on the heap, cannot be. `no_std`, no `unsafe`, and one dependency,
`lib/unicode`, which links no allocator either.

`lib/util`'s `utf8::shown` is the allocating form, a name answered as a
borrowed or owned string.
