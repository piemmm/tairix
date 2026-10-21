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

An executable — a TAIRiX program image, a driver, an ELF64 file or a
WebAssembly module — opens in the code view instead: what its headers say,
each part at its offset with its length and what it declares — the system
version it was built for, its entry point, its segments and the libraries it
needs, a driver's signed manifest with the capabilities it asks for and the
hardware it binds to, an ELF file's program and section headers, a module's
sections — then each code region's instructions, one a line with its address
and encoding bytes, labelled with the symbols they start and the symbols
their branches reach. Keys and signatures are shown as they are written, not
checked. The code view is
read-only; the text and hex views edit the same file. The code is decoded by
a second worker of its own, with no reach either, and read as the instruction
set the file names, else as this machine's; the View menu's Instruction set
reads it as another, and the view field of the status line offers the code
view beside text and hex.

A picture — PNG, JPEG, GIF, BMP, TIFF, WEBP, an icon or cursor file, a RISC
OS sprite area or an OpenRaster document — opens in the data view instead:
what the file is made of, each record at its offset with its length and what
it declares, every EXIF tag by name with its value, then every byte of the
file as hex beside its character. A signed manifest — an application's
`AppInfo` or a program's manifest — opens there too: what it names, the
capabilities it asks for, the file types it claims and the services it
browses, then its bytes. The data view is read-only; the hex view edits the
same bytes. A picture is read by a third worker of its own, a manifest by the
code view's, neither with any reach, and the View menu offers the data view
beside text and hex.

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

Pressing the secondary (right) mouse button anywhere in the window opens
its menu: Cut, Copy, Paste and Select all, then File, Edit, Find and View,
each opening its own submenu. The window has no menu bar.

The status line shows the caret's line and column, what the checker found,
and, as fields that open a menu when clicked: the format, text or hex, the
line endings, and the indentation. Closing a window or quitting with changes
not saved asks first. Closing a window with the secondary (right) button on
its close button also shows its file in the file manager, in the folder that
holds it.

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
