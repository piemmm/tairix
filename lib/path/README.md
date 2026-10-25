# tairix-path

Shared filesystem path-spelling parser for TAIRiX (`lib/path`).

TAIRiX storage is a forest of named roots, not one global Unix tree, so a path
string names its root explicitly. Several components need to turn such a string
into a structured, validated form — the shell first (`cd`, prompt display, word
and tilde expansion, completion), and later the file browser and file-management
tools. That lexing and normalisation is identical wherever it happens, so it
lives here once and every consumer imports it, rather than each embedding a
private path parser. The shell does not own a path parser; it links this one.

## Three spellings beside the parser

`leaf_name(path)` is the POSIX base name and `dir_name(path)` the POSIX dir
name (trailing `/`s ignored, a bare root naming itself, a lone relative
component in `.`, and a `Name:/` alias root playing exactly the role POSIX
gives `/`), and `join(parent, name)` appends a child with exactly one
separator so a child of a root is `/name` and never `//name`. All three are
purely lexical, like `parse`: they resolve nothing and open nothing. They live
here because more than one consumer needs them — a tool that names the link
`ln -s target` creates, `dirname`, `cp` telling a source named twice from
another name for its node, a browser that spells a resolved link target — and
a private copy per consumer is the duplication the charter forbids.

## A spelling step, not a resolver

This crate turns a string into a typed `Path` (a `Root` plus normalised
components). It does **not** resolve an alias to a volume, open anything, or
perform a capability check. A resolved name is still subject to inode
permissions, ACLs, capability gates, mount flags, and MAC policy at open time.
Resolving a root handle and opening a descriptor belong to the storage and
filesystem subsystems, so parsing a string here can never widen authority.

## Accepted spellings

- **Synthetic view** — `/Users/ian/Documents` (`Root::View`).
- **Alias shorthand** — `Home:/Documents/spec.md` (`Root::Alias`). The first `/`
  after the `:` marks the root boundary; the alias name is not a component.
- **Expanded alias** — `alias::Home/Documents` (the normalised internal spelling
  of the same alias path; parses to the identical `Root::Alias`).
- **Relative** — `Documents/spec.md`, `.`, `../notes` (`Root::Relative`),
  resolved by the caller against a current directory.

A leading `Name:` (single colon) not followed by `/` is refused with
`PathError::NotAPath`: that is a resource reference (`namespace:selector`, owned
by the separate resource-reference grammar) or an alias path missing its `/`.
The durable/administrative resolver spellings (`id::`, `fs::`, `<driver>::`,
`dev::`, `net::`) are refused with `PathError::UnsupportedResolver` — they have
no consumer yet and are added by the stage that introduces their callers, rather
than being invented speculatively here.

## API

- `parse(input) -> Result<Path, PathError>` — a spelling's root and its
  normalised components.
- `steps(input) -> Result<(Root, Steps), PathError>` — the root, and the
  steps as spelled read one at a time, `.` dropped and `..` kept as
  `Step::Up`, for a resolver that applies each `..` to the directory it
  actually reached and holds the steps under a bound of its own. Names are
  checked exactly as `parse` checks them, and the first refusal ends the
  steps.
- `Path::root() / components() / alias() / volume_id() / is_absolute() /
  prefix(len)` and a `Display` that renders the canonical human spelling.
- `validate_file_name(name) -> Result<(), PathError>` — the one shared rule for
  a single **leaf name** (one file or directory name, not a path): non-empty,
  not `.`/`..`, no `/`, no control character or `:`, within `MAX_COMPONENT_LEN`
  (the kernel's `FS_NAME_MAX`). A tool that lets the user *name* something (the
  file manager's rename) judges the typed name against this so every consumer
  agrees on what a legal name is.
- `root_len(input) -> Result<usize, PathError>` — the byte length of the root
  a spelling names (`/`, `Name:/`, `alias::Name/`, `id::<volume-id>/`; `0`
  when relative), by the same rule `parse` applies, examining nothing after
  it: the shell's filename generation splits a word's root off before reading
  the rest as a pattern.
- `alias_root_len(input) -> Option<usize>` — the byte length of a leading
  `Name:/` alias-root prefix, so a lexical tool (`basename`, `dirname`) treats
  an alias root as POSIX treats `/` without resolving anything.
- `is_relative_spelling(input) -> bool` — whether the grammar reads a
  spelling as relative to the working directory: no leading `/` and no `:`
  before its first `/`. A listed name holding a `:` (a foreign volume's
  `Home:`) would read as a root leading a relative path, so the shell's
  filename generation writes such a path from `./`.
- `is_hidden_name(name) -> bool` — whether a directory entry is a hidden item
  a listing leaves out unless asked for everything: a name beginning with `.`.
- `PathError` — why a string or name was rejected.
- `MAX_PATH_LEN` / `MAX_COMPONENTS` / `MAX_COMPONENT_LEN` / `MAX_ALIAS_LEN` — the
  fixed security bounds on an untrusted path string; the path and component
  lengths are the filesystem ABI's own (`FS_PATH_MAX`, `FS_NAME_MAX`).

## Design

- `no_std` + `alloc`, `#![forbid(unsafe_code)]`.
- Fail-closed: a malformed or over-long path is a typed `PathError`, never a
  silently "fixed up" path. It parses untrusted input, so length, component
  count, component size, and alias size are fixed security bounds, not growable
  capacities.
- `..` cannot climb above a view or alias root (`EscapesRoot`); a leading `..`
  in a relative path is preserved for the caller to resolve.
- Linear time, no recursion: neither a hostile path nor a hostile component can
  trigger runaway work. Parsing never panics.

## Stability

Tier: `experimental`.
