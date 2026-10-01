# `Paint` — the desktop image editor

`Paint.app` paints and edits pictures pixel by pixel or with brushes and
shapes. It reads every picture format TAIRiX decodes and writes PNG, JPEG and
RISC OS sprite files, editing each depth as it is stored: 1, 2, 4 and 8-bit
palettes and 32-bit colour. A sprite with no palette of its own shows the
RISC OS desktop's (Wimp) colours, never a PC palette; a sprite the editor
cannot read is kept byte for byte and saved back unchanged. The design is
`plans/PAINT.md`.

Stability tier: **experimental**.

## Capabilities

`CAP_CONSOLE_WRITE`, `CAP_SHM`, `CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT` — and **no
filesystem capability**. A document reaches the painter only as the user's own
act: a descriptor Files or the desktop opened for it, or a grant the session's
trusted picker delegated. The manifest declares `document-access =
"read-write"`, so a document the user may write is handed over writable and
Save writes back through it — but only in a format Paint writes, and never
over a 16-bit PNG it narrowed to 8 bits on the way in; any other is read-only,
and Save asks where.

`CAP_SANDBOX_SPAWN` re-enters the binary as a capability-empty worker, the
one process it may start. Documents
and pasted pictures are untrusted input, so none of their bytes are parsed in
the painter's address space, and each document is decoded by a fresh worker
that is released once it has been read.

## What runs where

The engine (`src/lib.rs` and its modules) is host-tested and does no I/O. The
`Run` binary keeps everything that waits, or grows with the picture rather
than with the brush, off the window's loop: a decode worker reads documents
and pastes through the sandbox, and a work queue encodes and writes saves,
encodes copies for the clipboard, and carries out fills and transforms.

## Limits

- A picture is at most 65,536 pixels on a side and 64 Mi pixels in all, and a
  document file at most 64 MiB: the sandboxed decode's own bounds.
- A sprite file holds at most 4,096 sprites.
- The undo history keeps every step at normal memory pressure, twice the
  document's pixels at mild pressure, once at moderate, and nothing beyond.
