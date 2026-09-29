# `TextEdit` — the desktop editor

`TextEdit.app` edits any file. Text shows every byte it holds: a control byte
reads `[x03]`, a byte that is not UTF-8 `[xC3]`, and an invisible or
direction-changing character `[U+202E]`, each in its own colour and one caret
stop. The hex view edits the same bytes. Source files are coloured, and the
TAIRiX settings stores are coloured and checked against the parser the system
reads them with. The design is `plans/TEXTEDIT.md`; the curses `edit` command
is a separate program.

Stability tier: **experimental**.

## Capabilities

`CAP_CONSOLE_WRITE`, `CAP_SHM`, `CAP_PROC_SPAWN` — and **no filesystem
capability**. A document reaches the editor only as the user's own act: a
descriptor Files or the desktop opened for it at launch or on a drop onto its
icon-bar slot, or a grant the session's trusted picker delegated. The manifest
declares `document-access = "read-write"`, so a document the user may write
is handed over writable and Save writes back through it; any other is
read-only, and Save asks where.

`CAP_PROC_SPAWN` re-enters the binary as a capability-empty worker: colouring,
format detection and settings validation parse untrusted bytes, so none of it
runs in the editor's address space.

## What runs where

The engine (`src/lib.rs` and its modules) is host-tested and does no I/O. The
`Run` binary keeps everything that waits off the window's loop: a document
worker reads, saves, searches and converts line endings over a snapshot of
the piece table, and a syntax worker drives the sandbox. A save writes the
snapshot through the document's descriptor, cuts the file to length and syncs
it.

## Limits

- A document is held in memory; one larger than the allocator will give
  refuses to open and says so.
- One edit copies at most 16 MiB on the loop (`MAX_EDIT_BYTES`); a larger
  paste or replacement is refused with the reason stated.
- A file whose name the registry cannot type (`Makefile`) is offered by no
  association, so Files and a drop do not hand it to the editor; its own
  Open… reaches it.
