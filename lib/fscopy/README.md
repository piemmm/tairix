# tairix-fscopy

The one file copy every copying tool runs — `cp`, `mv`, `fstree`, the file
manager, and the runtime's `replace_file`: a file's bytes with its holes kept
(`DataCopy`, GNU `cp --sparse` semantics over the hole-aware seek), its
extended attributes carried as far as the destination can keep them
(`copy_attributes`), and one report of what could not be (`Omissions`).

The engine reaches files only through its `Source`, `Target` and
`AttributeStore` seams, so it is host-tested against fixtures; a tool reaching
files by path offers them as `PathFiles`, each a `PathFile`. The running
system's seams are `tairix_rt::copy`. `docs/src/lib/fscopy.md` describes the
semantics.

## Stability

**experimental.**
