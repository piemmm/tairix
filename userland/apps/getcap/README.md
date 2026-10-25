# `tairix-getcap` — report a file's capability gate

Stage 6 deliverable (`AGENTS.md` §3 `userland/apps/`). `getcap` reports
the **optional capability requirement** an inode may carry: a capability
the caller must hold to reach the node at all, on top of the mode/ACL
checks (`AGENTS.md` §5.3). For each file operand it prints one line —
`path CAP_NAME` — when the file carries a gate, and prints nothing for a
file that has none, so a clean tree is silent. With `-R` a directory
operand is reported and then its contents recursively. It is the
read-only companion of `tairix-setcap`.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths. It depends only on
`lib/*` crates, so it never links a kernel or driver crate.

## Usage

```
getcap [-R] [--] file...

  -R, --recursive  report files and directories recursively
  -h, --help       show the usage banner
```

At least one file is required. A long option may be shortened to any
unambiguous prefix (`--rec`). `--` ends option parsing: every later
argument is an operand. `getcap` spells recursive `-R`; a bare `-r` is
not an option.

## Capability names

A gate renders by its canonical `CAP_*` name (e.g. `CAP_AUDIT_READ`),
resolved through `tairix_abi::CapabilityId::name` — the single, frozen
`abi-v1` source of truth shared with `setcap` (`AGENTS.md` §2.2, §5.2). A
node that stored an in-range identifier the running ABI has not yet named
renders as `CAP_<id>` rather than being silently dropped, so a gate is
never hidden (`AGENTS.md` §2.1).

## A reporter, not a policy point

`run` opens each operand through its final link, reads its gate through
that descriptor, renders the gated files, and walks each directory `-R`
must descend (reporting the directory before its contents). The driver
only *reports* the stored gate; `getcap` makes no permission decision
(`AGENTS.md` §5.4 — the VFS is the policy point). The operations that
reach the outside world are injected seams:

- `Roots` — the shared tree seam (`tairix_fscopy::tree`): each operand
  opened through its final link; each tree walked through the directories
  the run holds open, each entry opened by name from its own directory and
  never through a link.
- `Gates` — read the gate of a node the run holds, through that node.
- `Output` — write the report and the usage banner to the terminal.

No binary ships `getcap` yet: the kernel has no call that reads a node's
gate (plans/APPS.md §12.1). The engine runs against `tairix_fscopy`'s
in-memory tree in its tests, so every parsing, rendering, recursion and
race decision is testable without a kernel.

## Fail closed

An option `getopt` refuses (`GetcapError::Option`, in its words) or a
missing operand (`GetcapError::Usage`) reports nothing. An operand that
cannot be opened surfaces the underlying `Errno` as `GetcapError::Stat`,
and one that is a link naming nothing is `GetcapError::Dangling`
(`chmod`'s `cannot operate on dangling symlink 'x'`); a gate that cannot be read is
`GetcapError::Query`; a directory that cannot be opened or listed during
a recursive descent is `GetcapError::Read`; a failed write is
`GetcapError::Output`. A file that cannot be opened, queried or listed is
reported and the rest are still reported, the rest of its directory too,
and the run exits 1. A link operand reports what it names; a link met
during a recursive descent — listed as one, or swapped in since — is left
alone. There is no panic.

## Tests

`cargo test -p tairix-getcap` drives the parser and the engine against
the shared in-memory tree and a recording output: the command grammar, a
gated file reported by name, an ungated file producing no output, an
unnamed in-range gate rendered numerically, several files reporting only
the gated ones in order, the non-recursive and recursive descent orders,
a link in the tree left alone, a link operand reporting what it names, a
dangling operand, an entry swapped for a link before each seam call of
the walk in turn never read through, and the missing-operand / stat /
query / read-during-recursion fail-closed paths.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md)
for the full subsystem documentation.
