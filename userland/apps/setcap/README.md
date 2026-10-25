# `tairix-setcap` — set or clear a file's capability gate

Stage 6 deliverable (`AGENTS.md` §3 `userland/apps/`). `setcap` changes
the **optional capability requirement** of each of its file operands: a
capability the caller must hold to reach the node at all, on top of the
mode/ACL checks (`AGENTS.md` §5.3). The capability operand is either a
canonical `CAP_*` name (e.g. `CAP_AUDIT_READ`), which installs that gate,
or the literal `-`, which clears the gate so the node has none. With `-R`
a directory operand is changed and then its contents recursively. It is
the policy-*writing* companion of `tairix-getcap`.

`setcap` stores the gate but makes no permission decision itself
(`AGENTS.md` §5.4 — the VFS is the policy point). Setting a gate is itself
a privileged operation; the filesystem seam refuses an attempt the caller
is not authorised to make (it surfaces as `SetcapError::Apply`).

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths. It depends only on
`lib/*` crates, so it never links a kernel or driver crate.

## Usage

```
setcap [-R] [--] CAP file...

  -R, --recursive  change files and directories recursively
  -h, --help       show the usage banner
```

A capability spec and at least one file are required. A long option may
be shortened to any unambiguous prefix (`--rec`). `--` ends option
parsing: every later argument is an operand. `setcap` spells recursive
`-R`; a bare `-r` is not an option.

## The capability grammar

The capability spec is one of:

- a canonical `CAP_*` name (`CAP_FS_MOUNT`, `CAP_AUDIT_READ`, …) — install
  that gate; the name is resolved through
  `tairix_abi::CapabilityId::from_name`, the same frozen `abi-v1` table
  `getcap` renders with (`AGENTS.md` §2.2);
- the literal `-` — clear the gate.

The name match is exact and case-sensitive (`AGENTS.md` §2.1 — no
guessing): an unknown, mis-cased, or bare-numeric value is rejected as a
`SetcapError::BadCapability` rather than coerced.

## A gate-setting machine, not a data source

`run` opens each operand through its final link, sets the new gate
through that descriptor, and walks each directory `-R` must descend
(changing the directory before its contents). The operations that reach
the outside world are injected seams:

- `Roots` — the shared tree seam (`tairix_fscopy::tree`): each operand
  opened through its final link; each tree walked through the directories
  the run holds open, each entry opened by name from its own directory,
  never through a link, and changed through that one descriptor.
- `Gates` — install or clear the gate of a node the run holds, through
  that node.
- `Output` — write the usage banner to the terminal (`setcap` is silent
  on success).

No binary ships `setcap` yet: the kernel has no call that sets a node's
gate (plans/APPS.md §12.1). The engine runs against `tairix_fscopy`'s
in-memory tree in its tests, so every parsing, cap-spec, recursion and
race decision is testable without a kernel.

## Fail closed

An option `getopt` refuses (`SetcapError::Option`, in its words) or a
missing operand (`SetcapError::Usage`) changes nothing; a capability
operand that is neither a known `CAP_*` name
nor `-` is a `SetcapError::BadCapability`. An operand that cannot be
opened surfaces the underlying `Errno` as `SetcapError::Stat`, and one
that is a link naming nothing is `SetcapError::Dangling` (`chmod`'s
`cannot operate on dangling symlink 'x'`); a gate that cannot be applied is
`SetcapError::Apply`; a directory that cannot be opened or listed during a
recursive descent is `SetcapError::Read`. A file that cannot be opened,
changed or listed is reported and the rest are still changed, the rest of
its directory too, and the run exits 1. A link operand changes what it
names; a link met during a recursive descent — listed as one, or swapped
in since — is left alone. There is no panic.

## Tests

`cargo test -p tairix-setcap` drives the parser and the engine against
the shared in-memory tree and a recording output: the command grammar,
the cap-spec parser (the named and `-` forms, and the unknown /
mis-cased / numeric refusals), a named-capability install, a `-` clear,
several files, the non-recursive and recursive descent orders, a link in
the tree left alone, a link operand changing what it names, a dangling
operand, an entry swapped for a link before each seam call of the walk in
turn never carrying the gate, and the missing-operand / stat / apply /
read-during-recursion fail-closed paths.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md)
for the full subsystem documentation.
