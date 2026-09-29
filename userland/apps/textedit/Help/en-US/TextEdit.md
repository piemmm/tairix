## NAME

TextEdit — graphical text and hex editor

## SYNOPSIS

`TextEdit`

## DESCRIPTION

Edits any file in a desktop window: text, source code, the system's settings
files, or raw bytes. Launched with a document — from the file manager, from
the desktop, or by dropping a file on its icon in the icon bar — it opens a
window on that file. Launched on its own it opens an empty window. Each
document is a window of the one editor; closing the last leaves it on the
icon bar, and the Quit row of its icon menu ends it.

Nothing a file holds is hidden. A control byte is drawn as `[x03]`, a byte
that is not valid UTF-8 as `[xC3]`, and an invisible or direction-changing
character as `[U+202E]`, each in its own colour and each one caret stop. A
file that looks like binary data opens in the hex view, which shows every
byte as two hex digits beside its character and edits the same bytes the
text view does.

Source files are coloured: HTML, XML and SVG, CSS, JavaScript, JSON, YAML,
TOML, Markdown, Rust, C, Java, Python and shell scripts. The system's settings
files — application settings, the program library, the system and network
configuration, the service overrides, the users and groups databases, and
font family manifests — are coloured too, and checked with the parser the
system reads them with: a problem is marked in the margin beside its line and
stated in the status line. The format is chosen from the file's name, then
from its opening bytes; a format chosen from the View menu or the status line
always wins.

The editor holds no filesystem capability. It edits only the file it was
handed. A file the user may change is handed over writable, and Save writes
it back; any other is read-only, and Save asks where to save a copy.
Colouring, format detection and checking run in a separate worker process
with no reach at all, so a hostile file cannot touch anything the editor can.

The status line shows the caret's line and column, what the checker found,
and, as fields that open a menu when clicked: the format, text or hex, the
line endings, and the indentation. Closing a window or quitting with changes
not saved asks first.

* `Ctrl+N` — a new window
* `Ctrl+O` — open a file
* `Ctrl+S` — save; `Ctrl+Shift+S` — save as
* `Ctrl+W` — close the window
* `Ctrl+Z` — undo; `Ctrl+Shift+Z` or `Ctrl+Y` — redo
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cut, copy, paste
* `Ctrl+A` — select everything
* `Ctrl+F` — find; `Ctrl+H` — replace
* `F3` / `Shift+F3` — the next or previous match
* `Ctrl+L` — go to a line
* `F8` — the next problem the checker found
* `Ctrl+]` / `Ctrl+[` — indent or outdent the selected lines
* `Ctrl+/` — comment the selected lines out or back in
* `Ctrl+Shift+H` — switch between the text and hex views
* `Insert` — switch between inserting and overwriting
* `Tab` — in the hex view, move between the hex and character columns

## OPTIONS

`-h`, `-?`, `--help`
: Write this help to the standard output and exit.

## EXIT STATUS

Zero after Quit. Non-zero when the window channel, the event mailbox, or the
desktop session was refused; the reason is stated on the standard error
stream.
