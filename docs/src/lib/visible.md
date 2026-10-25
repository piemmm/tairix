# tairix-visible

`lib/visible` holds the one rule for the characters that draw nothing where
they stand yet can hide, reorder or break the text around them, so text from
elsewhere is shown with each replaced by U+FFFD and what is drawn is what is
there:

- **`is_hidden(ch)`** — a C0 or C1 control, a line or paragraph separator, an
  interlinear-annotation control, or a default-ignorable code point (Unicode
  `DerivedCoreProperties.txt`): the bidirectional controls, the zero-width
  characters, the fillers, the variation selectors and the tags.
- **`shown_char(ch)`** — one character as it is shown, U+FFFD for a hidden one.
  `lib/vt`'s emitter prints every glyph through it, so text from elsewhere never
  reaches a terminal as a command or rearranges what it shows, and `lib/curses`
  holds each cell to it.
- **`Shown(value)`** — any `Display` formatted with each hidden character
  replaced, straight into its destination and without allocating, which a GUI
  application's `stderr` reports pass through.

TextEdit draws hidden characters as `[U+202E]` tokens, `lib/net`'s
`DisplayLabel` escapes them in what a peer names itself, `lib/devids` and
`lib/tz` refuse a database that holds one, and the font client and `fontd`
answer one with an empty glyph — all by this rule.

The crate is `no_std`, has no dependencies and links no allocator under any
feature, because every architecture port's boot console links `lib/vt`, and a
QEMU vertical links a port with no allocator at all. `lib/util`, whose members
hold text on the heap, cannot be that home: cargo unifies a crate's features
across one build, so an edge that asks `lib/util` for no allocator is overruled
by any other crate in the same build that does. `lib/util`'s `utf8::shown` is
the allocating form, a name answered as a borrowed or owned string.
