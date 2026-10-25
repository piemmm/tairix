# tairix-fscopy

The one file copy every copying tool runs — `cp`, `mv`, `fstree`, the file
manager, and the runtime's `replace_file`: a file's bytes with its holes kept
(`DataCopy`, GNU `cp --sparse` semantics over the hole-aware seek), its
extended attributes carried as far as the destination can keep them
(`copy_attributes`), one report of what could not be (`Omissions`), the
modes what a copy makes is given (`file_mode`, `DirModes`) and those a move
carries (`carry_owner`), the check that a tree copy never lands inside itself
(`landing`, `Descent`), the record that keeps one operand from replacing
another's destination (`Created`), and the one resumable copy, move and
removal of a tree (`op`), which `fstree` and the file manager run.

It also carries the shared tree seam (`tree`): walking a tree through
directories held open, reaching each child by name from there and never
through a link it was not asked to follow, `Cursor`, the depth-first walk with
its place kept between calls, and `walk`, the loop over it. The engine reaches files only through held nodes, so it is
host-tested against the `fixture` tree (feature `test-support`), whose
attacker hook renames, replaces or relinks a name between two calls; the
feature's `resolve` is the kernel's path resolution in miniature for a test
filesystem keyed by path. The running system's seams are `tairix_rt::copy`.
`docs/src/lib/fscopy.md` describes the semantics.

## Stability

**experimental.**
