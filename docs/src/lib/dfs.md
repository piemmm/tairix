# `tairix-dfs`

`lib/dfs` is the Acorn DFS on-disc format as DFS 1.20 writes it. The volume
probe (`lib/fsprobe`), the [archive driver](../filesystem/archive.md) that
serves a DFS disc image as a volume, and the Open Micro emulator's DFS ROM,
blank discs and scenario discs reach the format here rather than each
carrying a copy.

| Module | What it is |
|---|---|
| `catalogue` | The two sectors opening each side: the title, the cycle number (`next_cycle`), the boot option, the side's sector count and its 31 entries (`Entry`), every field's offset, and the OS address an entry's 18-bit load or execution field is (`address`, `field`). |
| `name` | The bytes a name DFS makes may hold (`admits`), the bytes a catalogue read is believed with (`readable`), and a name's padded spelling. |
| `image` | Where a side's bytes lie in an image (`Layout`): an SSD's tracks one after another, a DSD's two sides alternating track by track, how much of a side a short image holds, and how long an image must be to hold a side's bytes. |

## Believing a catalogue

`Catalogue::read` believes a catalogue only where a DFS disc could hold it:
an entries' end that is a whole number of entries, no option bit DFS 1.20
leaves clear, a side of at least the catalogue's own two sectors, a title
free of control codes, and every entry a name and directory of printable
bytes lying between the catalogue and the side's end. The probe recognises a
DFS image by its first catalogue alone. A name's top bits, which some
protection schemes set, are believed and kept.

## Addresses

An entry keeps eighteen bits of each address. Both top bits set mark the I/O
processor's, which the OS gives as `&FFFFxxxx`; `field` refuses any address
no eighteen-bit field reads back as.

## Images

An image may end short of its disc's last track: the missing tail is
unrecorded sectors, read as zeros. `Layout::image_end` is how long an image
must be to hold a side's first bytes in whole sectors, which is as far as a
writer grows one.

## Verification

Each module's tests hold it to hand-worked values; the fuzz target
(`tests/fuzz_dfs.rs`) drives the catalogue and entry decoders with
well-formed catalogues mutated and with noise, checking that a believed
catalogue lays every file inside its side and writes back as itself, that
every entry encodes back to its bytes, that each layout places a side's bytes
apart from each other and from the other side's, and that every address field
widens and narrows back to itself.
