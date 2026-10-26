# tairix-visible

`lib/visible` holds the one rule for showing text from elsewhere so that what
is drawn is what is there: each character that draws nothing where it stands
yet can hide, reorder or break the text around it is replaced by U+FFFD, and a
combining sequence is cut at one U+FFFD where it would carry more than four
marks or one twice (UTS #39 §5.4):

- **`is_hidden(ch)`** — a C0 or C1 control, a line or paragraph separator, an
  interlinear-annotation control, or a default-ignorable code point: the
  bidirectional controls, the zero-width characters, the fillers, the variation
  selectors and the tags. It is read from [`lib/unicode`](unicode.md)'s tables,
  generated from the pinned Unicode Character Database, so the set is never a
  hand-kept copy.
- **`shown_char(ch)`** — one character shown on its own, U+FFFD for a hidden
  one. `lib/vt`'s emitter prints every glyph through it, so text from elsewhere
  never reaches a terminal as a command or rearranges what it shows, and
  `lib/curses` holds each cell to it.
- **`Shown(value)`** — any `Display` formatted as text from elsewhere is shown,
  hidden characters and over-long or doubled mark stacks escaped, straight
  into its destination and without allocating: window titles and a GUI
  application's `stderr` reports pass through it. **`shown_chars(text)`** is the
  same characters, lazily, for a surface laying them out under its own budget.
- **`Faithful`** — the lossless twin for the renderings that escape rather than
  replace: which characters such a rendering escapes. `lib/net`'s
  `DisplayLabel`, GNU quoting in `lib/util` and the log renderer escape by it.

TextEdit draws hidden characters as `[U+202E]` tokens, `lib/devids` and
`lib/tz` refuse a database that holds one, and the font client and `fontd`
answer one with an empty glyph — all by this rule.

The crate is `no_std`, holds no `unsafe` and links no allocator under any
feature, because every architecture port's boot console links `lib/vt`, and a
QEMU vertical links a port with no allocator at all. Its one dependency,
`lib/unicode`, links none either. `lib/util`, whose members hold text on the
heap, cannot be that home: cargo unifies a crate's features across one build,
so an edge that asks `lib/util` for no allocator is overruled by any other crate
in the same build that does. `lib/util`'s `utf8::shown` is the allocating form,
a name answered as a borrowed or owned string.
