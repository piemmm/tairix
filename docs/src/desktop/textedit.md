# TextEdit

`TextEdit.app` (`userland/apps/textedit`, `plans/TEXTEDIT.md`) is the desktop
editor for any file. It is one resident instance with a window per document;
closing the last window leaves it on the icon bar, whose menu offers a new
window and Quit. The curses `edit` command is a separate program.

## What a window shows

A document is bytes, never a decoded string, so any file opens and invalid
UTF-8 and control bytes round-trip exactly.

- **Text.** UTF-8 in a monospace grid, measured by `tairix_vt`'s width rule.
  A control byte is drawn as `[x03]`, a byte that is not UTF-8 as `[xC3]`,
  and a bidirectional or zero-width format character as `[U+202E]`, each in
  its own syntax colour and each one caret stop — an editor that hides those
  can be made to show code other than the code that runs. A line longer
  than `MAX_ROW_BYTES` continues on further rows, each scrolled through like
  any other: the document counts the rows its lines take, so the scroll bars
  span rows and columns in pixels and a wheel moves as far as its pixels
  reach.
- **Hex.** Offset, sixteen bytes and their characters, with a caret in
  either pane. A document whose head looks binary opens here.
- **Format**, orthogonal to the mode: the colouring and checking the text
  view applies (`tairix-syntax`, [its page](../lib/syntax.md)). It comes from
  the file name, then from the head (asked of the sandbox), and a format the
  user picks from the View menu or the status band always wins.

A settings store is checked by the parser the system reads it with once its
edits pause (`CHECK_SETTLE_NS`); a problem is marked in the gutter beside its
line and stated in the status band, and `F8` walks them. The editor knows a
document only by its name, so a rule that depends on where the store lives —
a font family naming its own directory as its fallback — is left to the
store's reader.

## Documents and authority

The manifest requests no filesystem capability and declares
`document-access = "read-write"`. A document is one the user handed over:
from the file manager or the desktop at launch (`--document-writable` or
`--document` with the descriptor on standard input), to a running instance
as a `Document` target, dropped on its icon-bar slot (the file manager opens
it for the editor, `docs/src/desktop/wm.md`), or chosen in the session's
trusted picker. Each surface opens it read-write where the user may write
it and says which. Save writes back through the handed-over descriptor; a
read-only or untitled document's Save is a Save as, whose picker delegates a
write-only descriptor to a new or confirmed-replaced file.

## The loop

The window's loop reads nothing and waits on nothing. Two workers carry what
does wait:

- the **document worker** answers each job in turn from a queue that grows
  by a fixed share (`JOBS_PER_WINDOW`) as each window opens, where a newer
  search or conversion withdraws the one it replaces. It reads a document
  into its piece table, on to wherever the file now ends; saves a snapshot
  (write it at offset 0, cut the file to its length, sync); searches; and
  converts line endings. A read, a search and a conversion each go one
  `STEP_BYTES` stretch at a time, so a save queued meanwhile runs between
  steps and a window's work is put down when it closes; all but a read run
  over a snapshot, handed out again while the document is unchanged, so
  editing goes on meanwhile;
- the **syntax worker** holds the sandbox that lexes batches of visible
  lines, detects a format from a head, and validates a store.

A window saves one snapshot at a time. A save asked for while one is in
flight freezes the document as it then is and is written, in turn, once those
ahead of it land, so a Save as is adopted before the next save chooses a file;
repeated plain saves become one of the latest document, while every Save as
keeps its own. Closing the window writes them all at once, so no file the
chooser already made is left empty.
These decisions are the engine's (`tairix_textedit::file`) and host-tested.
The loop takes in every answer and every queued event, then paints each
window once, a keystroke repainting the rows it changed and the status band;
a present the desktop refuses is that window's alone, said once and repainted
whole at its next chance. The process does not end under a save still being
written, even when the desktop's channel is lost, and a save that fails after
its window has closed is still said on `stderr`.

A window's title is the document's name shortened to fit the title field
beside its marks, so a long or oddly spelt name never keeps a window from
opening.

## Limits

- A document is held in memory; one the allocator will not hold refuses to
  open and the window says why.
- One edit copies at most `MAX_EDIT_BYTES` on the loop; a larger paste or
  replacement is refused with the reason stated. Replace All takes at most
  `MAX_REPLACEMENTS` matches per run and makes them one change over the span
  they cover, every replacement naming one stored copy of its text.
- A file name the media registry cannot type is offered no application, so
  the file manager and a drop do not hand it to the editor; the editor's own
  Open reaches it.
- A save overwrites the file in place through its handed-over descriptor: the
  editor holds no authority over the folder, so it cannot write beside the
  file and rename. A save interrupted by a crash can leave the file partly
  written (`plans/TEXTEDIT.md` TE13).
