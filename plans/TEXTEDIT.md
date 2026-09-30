# TEXTEDIT — the desktop text editor (`TextEdit.app`)

Binding under `AGENTS.md`. What the desktop editor is, the desktop facilities
it needed that did not exist, and the seams that keep parsing out of its
address space and I/O off its loop.

## Ledger

| Id | Item | Status |
|---|---|---|
| TE1 | Syntax roles and their colours in the shared theme (`lib/theme`) | done |
| TE2 | Settings-format crates report the line of a refusal and expose their line grammar | done |
| TE3 | `lib/syntax`: the format set, detection, the lexers, validation over the real parsers | done |
| TE4 | `lib/sandbox::textsyntax`: the sandboxed syntax service, its protocol and fuzz harness | done |
| TE5 | Writable documents: `document-access`, read-write hand-over from Files, the desktop and the picker, `fd_grant` passing on the grantor's own reach | done |
| TE6 | The trusted picker's Save mode and the picked leaf name | done |
| TE7 | Drag-and-drop from Files onto an icon-bar slot | done |
| TE8 | The session clipboard | done |
| TE9 | The app-set pointer shape | done |
| TE10 | The editor engine: document, history, layout, highlighting cache, find, the view | done |
| TE11 | The `Run` binary: windows, the two workers, documents, menus, the icon bar | done |
| TE12 | The bundle: manifest, icon, Help in every required locale, docs, registration | done |
| TE13 | A save that a crash cannot tear | blocked: no VFS primitive replaces a file's content atomically through a held descriptor |

## What it is

`TextEdit.app` (`name = "TextEdit"`, `title = "TextEdit"`, `id =
os.tairix.textedit`, `kind = "application"`, `library = "Accessories"`): the
graphical editor for any file, text or not. The curses `edit` command is a
separate program and stays one; neither links the other.

Single instance, a window per document, resident on the icon bar. The manifest
requests `CAP_CONSOLE_WRITE`, `CAP_SHM`, `CAP_PROC_SPAWN` (the sandbox
worker) and `CAP_LOG_EMIT` (the record of a worker replaced after a crash), and
**no filesystem capability**: every document reaches it through a
user-mediated grant (Files, the desktop, the picker, a drop), and every write
goes through the grant the user's act conferred.

## The document

A byte sequence, never a decoded string: any file opens, invalid UTF-8 and
control bytes round-trip exactly, and the hex view edits the same bytes the
text view shows.

- **Storage** is a piece table arranged as an implicit treap: pieces of at
  most `MAX_PIECE` bytes over immutable shared chunks (`Arc`), each node
  summarising how its subtree's bytes break into lines — its bytes, line
  feeds, the runs before its first and after its last feed, and the rows its
  whole lines take. Insert, delete, offset→line, line→offset, a line's first
  row and a row's line are all logarithmic in the piece count; typed text
  appends to one active chunk. A snapshot seals that chunk and lists the
  pieces, so a save, a search or a conversion reads the document on a worker
  while editing continues; the editor hands the same snapshot out again until
  the document changes. Loading reads chunks and builds the tree and the
  editor on the worker. Replace All is one change over the span its matches
  cover, the text between them kept in its pieces and every replacement naming
  one stored copy.
- **Rows.** A line longer than `MAX_ROW_BYTES` continues on further rows, so
  no row's work grows with the line. Each row starts `MAX_ROW_BYTES` into the
  line after the one before, drawn back to the start of a character it would
  split, so a line's row count follows from its length and the tree can count
  rows; the scroll bars span rows and columns in pixels, and a wheel moves as
  far as its pixels reach, into a long line's own rows.
- **Lines** end at LF. The CR of a CRLF is part of the terminator and is not
  drawn; a lone CR is a control byte. New line breaks take the document's own
  convention (the first terminator found), and converting conventions is one
  undoable edit.
- **History** is the list of piece-level edits with the selection before and
  after, grouped by typing run; undo and redo restore pieces, never re-copy
  bytes. It is trimmed from the oldest group when memory pressure arrives or
  deepens; pressure easing trims nothing.
- **Every data-sized allocation is fallible.** A load, paste or replace the
  allocator refuses leaves the document as it was and states why.

## Display modes

- **Text.** UTF-8 scalars in a monospace grid (`tairix_vt::char_width` is the
  one width rule). Tabs advance to the tab stop. A control byte is drawn as
  `[x03]` in the control colour, an invalid UTF-8 byte as `[xC3]` in the
  invalid colour, and a bidirectional or zero-width format character as
  `[U+202E]` in the invisible colour — an editor that hides those can be made
  to show code other than the code that runs. A token is one caret stop.
- **Hex.** Offset, sixteen bytes, and their ASCII, with a caret in either pane;
  typing a hex digit writes a nibble, overwrite by default, `Insert` toggles.
- **Format** (orthogonal to the mode): the syntax colouring and validation
  applied in Text mode — plain text, HTML, XML/SVG, CSS, JavaScript, JSON,
  Markdown, Rust, C, Python, shell, TOML, and the TAIRiX settings formats.

## Menus

The window has no menu bar. A secondary press anywhere in it opens the one
window menu at the press: Cut, Copy, Paste and Select all on the plate it
opens with, then File, Edit, Find and View as submenus, View holding Format,
Tab width, Indentation and Line endings a level deeper. The press gives the
keyboard to the find field, replace field or text it lands on, and a row runs
through the same focus-aware dispatch as its shortcut, so the two cannot
disagree about their target. Every action is a row exactly once, which is what fills three plates' worth of the menu model. The
status band's fields open the format, mode, line-ending and indentation menus
on a primary press, and a refused menu is stated in the status band.

## Detection

On load: a document whose head has a NUL, or invalid UTF-8 or control bytes
past a fixed fraction, opens in Hex; otherwise Text. The format comes from a
settings store's fixed file name first (two different grammars share `.conf`,
and the editor knows a document by its name only, never its path), then the
extension (`lib/browse`'s media registry, the one extension table), then the
head (a doctype, an XML prolog, a shebang), asked of the sandbox. A file that
merely shares a store's name is read as that store until the user picks
otherwise: the user's choice from the mode or format menu always wins and is
remembered for that window.

## Highlighting and validation

Both parse untrusted bytes, so both run in a capability-empty sandbox worker
(`lib/sandbox::textsyntax`, the lexers in `lib/syntax`); the editor holds
answers, never a parser.

- A lexer is a total, bounded state machine over one line:
  `(state, bytes) -> (spans, state)`. The editor keeps each window's line
  states at sparse checkpoints and a valid frontier; an edit moves the
  frontier back to the edited line, and only lines between the frontier and
  what is on screen are asked for, in bounded batches. A line longer than the
  tokenisation bound is drawn plain past it.
- An answer names the document generation it was asked for; a stale one is
  dropped. Until colour arrives a line draws with its previous spans shifted
  by the edit, or plain.
- Validation runs the **real** parser of a settings format (`lib/sysconfig`,
  `lib/netconfig`, `lib/appconf`, `lib/proglib`, `lib/users`,
  `lib/fontface`, `lib/enrolment`) after edits settle, and reports what the
  system would do: the first refusal of a strict format with its line, and
  every line a tolerant one ignores. No grammar is restated in the editor or
  the lexers: each format crate exposes its own line shape. A rule that
  depends on where the store lives (a font family falling back to its own
  directory) cannot be judged from a document known only by its name and is
  left to the store's reader.
- A worker that fails is replaced by the seam; a document that fails it
  repeatedly draws plain in that window with the reason stated.

## Documents and authority

- An application declares `document-access = "read-write"` to be handed
  documents it may write; absent, it is handed read-only ones.
- Files, the desktop and the trusted picker open a document read-write for
  such an application when the user may write it, and read-only otherwise
  (`tairix_browse::document::open_for`); the editor shows which. A fresh
  launch is wired the descriptor with `--document-writable`, a live instance
  receives a `Target::Document` carrying `writable`, and a pick's
  `FilePicked` carries `writable`. The picker resolves the requester's claim
  from the bundle the kernel attests it runs, never from the request.
- `fd_grant` may pass on the grantor's own reach (`GRANT_EXTENT_INHERIT`)
  instead of a stated ceiling, exactly as spawn-time conferral already does; a
  delegation still never exceeds its grantor, and a stated ceiling still
  attenuates. The app-data service keeps passing its bounded ceilings.
- A save writes the snapshot at offset 0, truncates to its length and syncs,
  on the worker. A refusal leaves the document modified and states why.
- That write is in place, so a crash part-way leaves the file torn (TE13).
  Writing beside the file and renaming needs authority over its folder, which
  a handed-over document deliberately does not carry. The fix is a VFS
  operation that stages content against a held writable descriptor and swaps
  it in whole, implemented by each writable filesystem.

## The picker's Save mode

`PickFile` carries a purpose: Open, or Save with a suggested leaf name. Save
shows a name field and a Save button in the session's own window, confirms a
replacement, and concludes by opening the chosen path write-only under the
session's authority: created exclusively and never through a link when new,
and never truncated — the requester writes from the start and cuts the file to
what it wrote, so a save abandoned first loses nothing. A replacement is asked
about, and done, for one full path; a move to another folder ends the question.
The leaf name of any pick is disclosed to the requester through
`TakePickedName`; the path never is.

## Drag-and-drop

A drag starts only from the window holding the pointer grab with the primary
button down (`BeginDrag`, which carries only the item's name). The session
takes the grab, floats an input-transparent plate naming the item beside the
pointer, and lights an icon-bar slot whose application's associations cover
the item, asking each slot once as the pointer arrives. A release there ends
the drag dropped (`DragEnded`) and the source takes the chosen application
(`TakeDropTarget`), then opens the item exactly as its Open With would.
Anywhere else, or on `Escape`, another button or the screen locking, it ends
with nothing dropped. The session confers nothing: the source holds the
authority and the user chose the target.

## The clipboard

Session-owned (`userland/gui/session/src/clipboard.rs`, the app side
`tairix_window::clipboard`). A set or a get is honoured only from the window
the user is working in: the one holding keyboard focus that the seat last
carried a key or a button press to, since a window takes focus just by
opening. The payload travels through a region the app grants the session,
mapped as that caller's own delegation (`shm_map_from`), is bounded
(`CLIPBOARD_MAX_BYTES`), is either text (validated UTF-8) or octets, and the
replaced payload is zeroed.

## The pointer shape

`SetCursor { window_id, shape }` sets what the pointer shows over the
window's client area; the application restates it as the pointer crosses its
own regions. TextEdit shows the I-beam over the grid and the find fields and
the arrow elsewhere, asking only when the shape changes.

## The loop

Two workers, each its own desk and wake. The document worker is
`tairix_rt::work`'s queued form, answering each job in turn: it loads (read
to wherever the file now ends, not to the length it measured at open), saves,
searches and converts line endings. A load, a search and a conversion each
run one `STEP_BYTES` stretch per job, so a save queued meanwhile runs between
steps and a closed window's work is put down between them; a newer search or
conversion withdraws the waiting one it replaces, so the queue's room is a
fixed share per window (`JOBS_PER_WINDOW`) grown as windows open. A save is
never refused while its memory can be had. The syntax worker holds the
sandbox (lex, detect, validate), one job at a time.

A window's file (`tairix_textedit::file`) decides its saves and pickers,
host-tested: one save outstanding; a save asked meanwhile freezes the document
then and is written in turn once those ahead of it land, so a Save as is
adopted before the next save picks a file. Plain saves asked in a row become
one of the latest document; every Save as keeps its own, its file already
made. Closing writes them all at once, each where it would have gone. A document opened into a window replaces only one with nothing
under way. The loop takes in every answer and queued event, then paints each
window once; a keystroke repaints the rows it changed and the status band,
never the window, and a present the desktop refuses is that window's alone.
The process does not end under a save still being written, even when the
desktop's channel is lost. A window's title fits the document's name to the
title field.

## Invariants

- No filesystem capability; no parser of the document in the editor's address
  space.
- A paint reads nothing and allocates nothing proportional to the document.
- An answer carries its window and generation; one for a closed window or a
  superseded generation is dropped — but a save that failed is still said.
- A file grant the editor did not ask for, or asked for a window since
  closed, is let go at once.
- Closing a window or quitting with unsaved changes asks first; nothing is
  discarded silently.
