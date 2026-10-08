# FILES-INTERACTION.md — selection, drag-and-drop, creation and richer icons in the file manager

Binding under `AGENTS.md`. The file manager (`userland/apps/files`) over the
shared engine (`lib/browse`) gains the interaction a desktop file manager is
judged by: marquee and modifier selection, drag-and-drop between its windows
and onto the desktop, a New ▸ submenu, one window per folder, and icons that
show what a folder holds and what a picture looks like. Its type and tiles are
tightened, and the user's home gains a `UserFiles` tree. Names keep their
extension over two lines, a selected name renames in place when clicked, a
folder's icon fans out the files inside it, tiles leave real ground between
them, icons cast soft shadows, the marquee keeps up with the pointer, and
thumbnails persist across sessions on volumes that can say exactly when a
file's content changed.

Read first: `plans/NEW-FILEMANAGER.md` (the engine/app split this keeps: every
model in `lib/browse`, the app only wires and paints, and the window never
waits on a disk), `plans/NEW-MENUS.md` (session-owned menus and submenus),
`plans/ICONS.md` (asset tiers and the sandboxed decode cache),
`plans/USERS.md` (the home shape), `plans/POINTING.md`,
`docs/src/desktop/apps.md` (the launch funnel and open-target queue),
`docs/src/filesystem/arxfs-spec.md` and `plans/APPDATA.md` (FI24, FI25).

## Ledger

| # | Item | Status |
|---|---|---|
| FI1 | Item names one point smaller: the `ItemLabel` role every icon tile's name and every listing row is set in | done |
| FI2 | Window titles one point smaller, for every window | done |
| FI3 | Compact icon tiles: the name half as far below its picture, the tile only as tall as what it holds | done |
| FI4 | A bare open — the icon-bar slot, a launch naming nothing — lands in `UserFiles` | done |
| FI5 | The home shape: `UserFiles` replaces `Documents` and holds `Documents`, `Music`, `Pictures`, `Videos`, provisioned by one shared walk | done |
| FI6 | One window per folder: a request for a folder already open raises that window, under a user-attested activation | done |
| FI7 | An empty default selection, a press on empty space that clears it, Ctrl and Shift clicks, and every selected item drawn | done |
| FI8 | Marquee selection that selects live, and scrolls the listing when it reaches an edge | done |
| FI9 | Drag items between windows and onto the desktop: copy by default, move with Shift, the pointer badged with the operation | done |
| FI10 | The New ▸ submenu: Folder, and each document type an installed editor writes and can start empty | done |
| FI11 | A folder's icon shows the kinds of file inside it, as cards standing in its mouth | done |
| FI12 | Image thumbnails: an image file's picture is its own content, decoded in the sandbox | done |
| FI13 | The resumable listing: `fs_readdir` reads a directory a batch at a time under the POSIX stream contract (closes D674) | done |
| FI14 | Reduced decodes: every format `decode_fitted` reads decodes into its box with memory bounded by the output (closes D598) | done |
| FI15 | A lossy WEBP's fitted decode reconstructs a macroblock row at a time, holding no whole plane | done |
| FI16 | A tile's name is the whole name over two lines, cut in its middle when it outgrows them so its extension always shows | done |
| FI17 | Clicking the name of the one selected item renames it in place once the click can no longer be a double-click | done |
| FI18 | A folder's icon fans out the files inside it: photos as their own pictures, other files as their kind, repeated as the folder holds them | done |
| FI19 | The marquee keeps up: input drained before a paint, each damaged rectangle painted alone, and a window present carrying a rectangle list | done |
| FI20 | A tile's body is its picture and its name: the press target and the plate hug it, and the cell around it is ground a band starts on | done |
| FI21 | Select All (`Ctrl+A`) and Clear Selection (`Ctrl+Shift+A`) in the window's menu | done |
| FI22 | A thumbnail is framed by a one-pixel line around the picture's own bounds | done |
| FI23 | Every icon a tile draws casts a small soft shadow, cast once per picture and retained | done |
| FI24 | ARXFS keeps a per-file content generation, reported by `fs_stat` and every directory record | done |
| FI25 | Thumbnails persist in the file manager's own store, keyed by content generation, so a picture is decoded once per version | done |

## FI1 — item names one point smaller

Names are set in a new `TextRole::ItemLabel`: an item's name in a dense
collection view. It sits one point below `Body`: the ladder's sizes are line-box
heights, and Inter's line box is 2478/2048 of its em, so one point (4/3 px of
em at the reference density) is about 1.6 px of line box; the rung is two
pixels below body at the shipped base, 1.2 points of em.

- `IconTile` sets its name in `ItemLabel`, so every icon view — the manager,
  the trusted picker, the desktop's icon field, the greeter's accounts — names
  its items at one size.
- `TableRow` takes its text role from its owner (default `Body`); the browser's
  listing rows use `ItemLabel` but keep the body row pitch, so they stay on the
  places rail's row grid beside them. Dialog and Properties rows keep `Body`.

## FI2 — window titles one point smaller

`TextRole::WindowTitle` moves to the same rung as `ItemLabel`. The compositor
draws every title bar from that one role, so the change is global by
construction. The title bar's identity icon is sized off the title line and
follows it, which keeps icon and text aligned.

## FI3 — compact icon tiles

The tile's picture slot and name band stop being a fixed proportion of the
tile. An owner states how many name lines its tiles hold (`TileLayout`); the
tile reserves exactly that band at the bottom, a half-inset gap above it, and
gives the picture the rest, capped by the width. `grid_metrics` sizes the cell
from what it holds — the picture side the tiles have always drawn (seven thirds
of the body line), a half-inset gap, one name line, insets — so the cell is
78 px tall at the reference density instead of 90, with the picture unchanged.
The greeter states three lines, which keeps its disc and its three-line band.

## FI4 — `UserFiles` is where a bare open lands

The fallback ladder for a bare open is `<home>/UserFiles`, then `<home>`, then
the root view (`tairix_browse::bare_open_places`). It applies to the icon-bar
default action, the desktop role's bare open, and `files` with no operand; a
named location keeps its own ladder (the folder, then the same three), and each
refusal names the place tried next. The trusted picker starts at the same place
and climbs to the nearest listable folder above a start that is refused.

## FI5 — the home shape

`AGENTS.md` §16.3's home folder `Documents/` becomes `UserFiles/`, holding
`Documents/`, `Music/`, `Pictures/` and `Videos/`.

- The names are defined once in `lib/abi::home` (beside the program-store
  names), and `lib/users` builds the home shape from them: the flat
  `HOME_SUBDIRS` plus the nested `USER_FILES_SUBDIRS`. Every consumer — the
  places rail, the desktop's pinboard folder, the trash, the ray-trace album —
  imports them; none spells a home name itself.
- One walk provisions the shape (`tairix_users::provision_home_shape`) over a
  small seam each backend implements: the kernel's `CAP_USER_ADMIN` backing,
  `tools/mkimage`'s debug image, and the QEMU users-root fixture. The three
  private creation loops are deleted. An existing node is kept as it is; a
  non-directory where the shape wants a folder is skipped rather than failing
  the account change, matching the repair semantics already tested.
- Every folder the walk creates is `HOME_MODE` and owned by the account.

## FI6 — one window per folder

Restacking stays the session's: an application cannot raise a window on its
own say-so. `WindowRequest::ActivateWindow` raises and focuses one of the
caller's own windows only when the caller holds an **activation**: either one
of its windows already has the keyboard focus (the user is working in it), or
the window engine minted it a one-shot activation when it delivered a
user-driven application event — `AppBarDefault`, `AppBarMenu`, or
`OpenRequested`. An activation records the window holding the keyboard when it
was minted (`Activation::Granted`) and is void once the keyboard is anywhere
else, however it moved; it is spent by its first use and dies with the client.
A hand-over lends one only while the user is working in a window of the
caller's and nothing holds the seat; otherwise the target still arrives,
`Activation::Withheld`. Anything else is `PermissionDenied`, and refused raises
and withheld hand-overs are audited. The policy is the engine's; the session
answers which window holds the keyboard and performs the raise. While the lock,
the picker, or the elevation or confirmation prompt holds the seat
(`seat_held`), no raise, popup or pick is granted and a newly shown window opens
beneath the surface holding the keyboard.

The manager compares a request's location with each browser window's location
(the canonical component spelling every location passes through) before opening
anything; a match is activated instead of opened. A refused activation opens a
window after all, stating the refusal, so the folder still appears.

## FI7 — selection

- A listing opens with nothing selected; the focus cursor rests on the first
  entry but is not drawn, and the first arrow key selects it.
- A primary press on empty space clears the selection (and starts a marquee,
  FI8). A secondary press there keeps clearing it.
- Ctrl+press toggles one entry; Shift+press extends from the anchor; the drag
  of a selected entry carries the whole selection.
- Every selected entry is drawn selected, in both views. The context menu's
  item rows are enabled from the selection, never from the bare focus.
- The verbs that act on one entry (Open, Rename, Properties, Open With…) act
  on `Browser::chosen_index`, the one entry selected; with several selected
  they say "several items selected" rather than pick one. Cut, copy and delete
  act on the set. A navigation clears the selection; a reload, a reorder or a
  watch change carries it with its entries.
- A press inside a multi-selection holds it (so a drag carries it all) and
  collapses to the pressed entry on a release that did not drag; a right-click
  inside it keeps it.
- What a round repaints (`listing::ViewMark`) is the shown entries whose mark
  changed, so its cost is bounded by the window, not the listing.
- These are engine behaviours, so the trusted picker gets the same model.

## FI8 — marquee selection

A primary press on empty listing space arms a band; past the drag slop it draws
from the press point and every entry any part of which it covers is selected as
it grows (with Ctrl or Shift held at the press, added to the selection the
press kept). The band's anchor is held in layout coordinates, so scrolling
grows it (`tairix_browse::marquee`).

- Hit-testing is arithmetic over the layout — a row range in the list, a
  line-by-slot range in the grid, every flow — so finding what a band covers
  never walks the listing, and a sweep visits only the entries entering or
  leaving it (`BandCells::each_not_in`).
- Autoscroll: while the head is in the strip at either end of the item area
  (the pointer reaches the app clamped into the window under the window
  manager's implicit grab, so "past the edge" reads as "at the edge"), the
  listing scrolls at a speed proportional to the depth into the strip, by the
  elapsed time at that speed. Steps are paced by a one-shot deadline on the
  loop's park, armed only while a step is due and the listing can still move;
  there is no periodic timer.
- Release ends the band; Escape takes back what it selected. One press policy
  (`gesture::press_step`) governs the band and a pending drag arm: a live band
  holds the listing, every pointer event and every key, and a primary press
  proves its release went elsewhere, ending it; an arm that has not travelled
  lets go on a key, another button, a second press or a lost keyboard, so no
  menu or dialog that took its release leaves it to begin on hover. The
  strips the band autoscrolls in are measured on the window rectangle the
  head is in.

## FI9 — drag and drop

The session carries every drag. What it can drop on, and what it tells the
source, are new.

- **The drag carries items, not a name.** `BeginDrag` names the first item, the
  count, and whether the drag is one openable file (`DragItems`); an icon-bar
  slot is offered only to that last kind. A drag of a selected entry carries
  the selection.
- **The source decides every folder drop.** While the pointer is over one of
  the source client's own windows (the engine's attested ownership decides),
  or over the desktop, the session reports where — once per input batch,
  numbered, with `Shift` (`WindowEvent::DragOver`) — and the source answers
  (`WindowRequest::DragVerdict`: refuse, copy, or move). Over the desktop the
  session holds the folder the pointer is on — the Desktop folder, or a folder
  icon on it — for the report that named it (`QueryDragSpot`), so the source
  can refuse a drop into the folder the items came from or into one of the
  items themselves. One policy decides the operation: copy, or move with
  Shift (`tairix_browse::drop_operation`).
- **The pointer shows the verdict.** `CursorKind::DragCopy` and `DragMove` are
  the arrow with a copy or move badge, in the built-in set and every shipped
  set; the session holds the pointer in the shape the latest current answer
  calls for (`CursorController::hold`). Arriving somewhere new, or a change of
  `Shift`, shows the plain arrow until it is answered; an answer to a report
  made before either is not shown. The session resolves each motion to an
  allocation-free place (`DragPlace`) and builds what is there — a slot's
  target, a desktop folder's path — only on arrival.
- **The source performs the drop, as the pointer showed it.** On release the
  session concludes with a `DropSite` — an application slot, one of the
  source's windows and a point in it, or a desktop folder, with the operation
  the pointer showed — and the source runs the copy or move through the same
  interleaved, cancellable transfer Paste uses, under its own authority, after
  asking the policy again. No path of the source's and no authority crosses to
  the session; the only path that crosses is the session's own Desktop folder,
  which confers nothing.
- **Where it would land is lit.** The target window lights the folder under
  the pointer (`Browser::set_drop_mark`, `render::drop_folder_at`) and the
  desktop its folder icon, while the answer accepts it, in the one drop-target
  look every collection control draws (the hover wash and an accent outline).

## FI10 — New ▸

The context menu's New ▸ submenu sits above Properties and always opens: Folder
(`Ctrl+Shift+N`), then one row per `BlankDocument` — a type an installed
application both writes (`document-access = "read-write"`) and declares by name,
whose empty file is already a complete document (`MediaType::blank_noun`) and
which an extension names (`tairix_browse::blank_documents`, in registry order).
Declaring an ancestor type does not count: a `text/plain` editor opens Rust
source but is not a maker of it. The rows come from the bundle scan the menu
already uses; there is no list of types anywhere. Today that yields Folder and
Text Document.

- Ids: the commands, the Rename field, a block as long as a plate offers Open
  With candidates, Folder, then the documents by their position in the list
  the menu was given. A document the menu did not offer makes nothing.
- A document is created exclusively (`CREATE | EXCLUSIVE`), so no existing file
  is truncated, under the first free name (`NewEntry::suggest_name`:
  `New Text Document.txt`, then `New Text Document 2.txt`), found in one pass
  over the listing. Every New ▸ row and the New Folder tool run one path, and
  open the inline rename on what they made.

## FI11 — what a folder holds

The occupancy probe reads one `PROBE_BUF_LEN` batch of the folder's entries
(FI13 bounds what that costs) and classifies their names; no content is read.

- Files group into families (`Family`) — picture, text, document, audio,
  video, archive, program. The up-to-three most frequent families in the batch
  are the folder's `FolderSample`, each drawn as the icon of its most frequent
  kind there (ties go to the first seen), so a folder of JPEG photos shows the
  JPEG picture. Subfolders and unclassified files make no card: a folder
  holding only those is occupied with an empty sample and draws the filled
  folder.
- A folder with a sample draws a composite (`ArtworkKey::Folder`): the
  folder's back, a card per family, the folder's front (`folder-back.svg`,
  `folder-front.svg`). It is rendered once per (sample, side) on the artwork
  worker and cached like any artwork. A card whose kind has no artwork tier is
  that kind's glyph on a paper card in a fixed ink, so the composite does not
  depend on the theme. A back or front that cannot be drawn, or a side under
  `MIN_COMPOSITE_SIDE`, degrades the picture to the plain filled folder.
- Audio and video are media types and icon kinds of their own, with built-in
  glyphs; their raster masters are artwork dropped into `lib/icon/assets/`.
- The file manager and the desktop share one probe desk (`lib/browse`
  `Probes`) and one resolver (`resolve_occupancy`), so a folder looks the same
  in a window and on the pinboard. A worker reads each recorded set as one
  batch (`vfs::probe_batch`), one batch in flight at a time. A held answer
  survives one resolve pass, which bounds the desk by the screen; a reported
  change to a folder, or a fresh listing of its parent, drops what is held or
  in flight for it. A resolve reports an entry only when its picture moved.
  The trusted picker draws no artwork and does not probe.
- A refused probe is recorded (the folder draws plain) and never asked again.
  A desk with no worker answers that it does not probe, so folders draw plain
  rather than costing the loop a directory read.

## FI12 — thumbnails

An image file whose format the shared decoders read (`lib/image`: all nine) is
drawn as its own picture.

- `entry_icon` (`lib/browse`) asks a regular file whose `MediaType::thumbnail`
  names a reading for `IconRequest::thumbnail`: the request's own tier, ahead
  of the class picture. A sprite area, which carries no signature, is read as
  one because its name says so (`Reading::Sprite`). A link is not
  thumbnailed: its listed size and time are its own.
- The key (`ArtworkKey::Thumbnail`) is the path, the file's identity, size and
  modification time as listed, and the reading, with the side as for any
  artwork, so a changed or replaced file is decoded afresh and a refusal is
  cached under the same key. The reader opens the file without following a
  link, under the program's own identity (`ArtworkReader::open`, `RtDocument`);
  a listing naming no file identity, a file past `MAX_THUMBNAIL_BYTES`, or one
  whose open handle disagrees with the key is refused before a byte is read.
- The file streams to the capability-empty sandbox over the chunked document
  upload (`tairix_sandbox::imagerender::thumbnail`). `OP_THUMBNAIL` forecasts
  the decode's peak from the header and refuses one over
  `MAX_THUMBNAIL_PEAK_BYTES` before allocating, decodes fitted into the
  tile's square (FI14) under the viewer's source bounds, and centres it on
  transparent padding, never enlarged. The worker lets the file go once it is
  drawn.
- The artwork desk queues thumbnails apart (`next_thumbnail`), so every icon
  is served first; the file manager's reader serves them after the folder
  probes too, and each is shown as it lands rather than held for a batch. The
  inline resolver declines a thumbnail, so a program with no worker draws the
  class picture rather than reading a whole file on its loop. Only visible
  tiles ask, and the desktop draws its picture files the same way. After a
  pass over every surface drawing thumbnails the embedder sweeps
  (`ArtworkDesk::sweep_thumbnails`), withdrawing each one nothing asked for —
  still wanted, or drawn and never collected — so scrolling never queues
  decodes of pictures gone from view; `MAX_WANTED_THUMBNAILS` bounds what one
  burst between sweeps can queue. Folder probes are swept the same way
  (`Probes::sweep`, `MAX_WANTED_PROBES`), and a landed batch holds the next
  until the sweep.

## FI13 — the resumable listing

`fs_readdir` reads a directory a batch at a time, as `getdents` does, so a
listing costs the kernel what the caller's buffer holds rather than the whole
directory, and no listing is too large to read.

- `fs_readdir(fd, buf, len, from)` writes as many whole `DirEntry` records as
  fit from the open description's listing position and advances it. `0` is the
  end. `BufferTooSmall` means the next record alone does not fit and leaves the
  position where it was. `from` is `ReaddirFrom::Next` or `ReaddirFrom::Start`;
  `Start` restarts in the same call, which is how a watched description relists
  after a rescan. Any other value fails closed.
- The position lives in the open file description and never leaves the kernel,
  so there is no cookie to forge. It is fixed to the directory its first batch
  read (volume and node); a batch that finds another directory at the path
  fails with `Stale`, so one directory's cursor is never applied to another.
  Calls on one description serialise on it.
- A call reads at most `min(len, READDIR_BATCH_MAX)` (64 KiB) of records,
  staged and copied out after the mount's lock is released, so the lock is
  held for one batch. A listing is the directory's own entries, then the covered mount
  points beneath it that the volume holds no entry for, in name order.
- The stream contract is POSIX's: an entry present for the whole listing is
  returned exactly once; one added or removed meanwhile may or may not be.
  Every driver keeps it (`docs/src/abi/driver_traits.md`): ARXFS and FAT
  cursors are stable slot positions, ext4's resumes at the next record
  boundary, and ADFS, whose directories are sorted arrays that shift, resumes
  at its index while the directory is unchanged and after the name it last
  returned, which the driver trait passes, once it has shifted. Each driver
  confines a cursor to the directory it is applied to and terminates on a
  corrupt one.
- `lib/rt`: `Dir::read` is one batch and `Dir::read_all` lists from the start a
  batch at a time, with no ceiling and no doubling retry. Every lister uses
  them; the private grow-and-retry loops are gone, and the kernel's own
  listers iterate the same primitive.

## FI14 — reduced decodes

`decode_fitted` — and `decode_fitted_as`, for a sprite area, which carries no
signature — decodes every format to the size covering its box with memory
the result sets, and `decode_peak_bytes` forecasts that decode from the
headers before anything is allocated.

- JPEG keeps its DCT scaling and ICO its page choice.
- Every other format feeds its rows, in the order the file stores them,
  through `lib/raster`'s `RowReducer`, the area-average arm of `resample`: the
  result is byte for byte what `resample` makes of the whole decode at the
  covering size. A fitted decode admits what `decode` admits — a TIFF's
  reduced copy only where its primary page passes the same admission
  (`tiff::admit`) and is no larger than it — so the work is bounded with the
  memory, and a box that does not reduce the picture is `decode`.
- An interlaced PNG decodes only the Adam7 passes that complete the coarsest
  grid covering the box; an interlaced GIF frame only the passes whose rows
  the box needs.
- TIFF reads the smallest reduced-resolution copy of its page that covers the
  box at the page's shape, a strip streamed a row at a time and tiles a band
  at a time; a transposing orientation holds the page, whose stored rows are
  the picture's columns. OpenRaster reads its thumbnail where that previews
  the document and covers the box, and otherwise streams its merged image.
- WEBP streams its colours. A lossy picture holds a window of macroblock
  rows (FI15), and its alpha is unfiltered a row at a time against the row
  above, borrowed in place where uncompressed; a lossless picture's words are
  held, since a back-reference may reach any earlier pixel. An interlaced GIF
  frame is read at a step its top lies on. An OpenRaster member the archive
  compresses is forecast from its own PNG header, inflated alone.

## FI15 — a lossy WEBP a macroblock row at a time

A lossy WEBP's decoder reconstructs one macroblock row into a plane whose
bordering row is the unfiltered bottom row of the row above (with its
above-right samples), which is what the format predicts from. Each finished
row is copied into a window below the filtered tail of the row above and
loop-filtered there, one row behind; the window keeps six luma and four
chroma rows of the row above, the most the next row's filter reads and the
colours of that row wait on. A picture row is handed over once its luma and
the chroma it covers are settled. Nothing is held per frame beyond one row's
macroblock state, and the result is pinned bit-identical to the whole-frame
decode over drawn keyframes under every filter (`vp8_tests.rs`); no pinned
frame yet uses segments or filter deltas (`plans/OPEN-DEFECTS.md` D803).

## FI16 — a name keeps its extension

A tile names its entry exactly as the volume holds it; nothing is ever
stripped. What lost the extension was the cut: one line, elided at its end.

- `render::TILE_LAYOUT` reserves two name lines, in the manager's grid and on
  the desktop, and the cell grows by one line.
- `BitmapFont::wrap_with_cut(text, width, lines, cut)` breaks as
  `wrap_to_width` does — at whitespace, mid-word when a word outgrows the line —
  and with `Cut::Middle { keep }`, when the name outgrows its lines, cuts the
  **last** line in its middle: that line's start, the mark, and the name's end.
  The end takes half the room, or the width of the name's last `keep` bytes
  when that is more, so the extension survives whatever is cut. A line is
  `TextLine { text, elided, tail }`; the mark sits between `text` and `tail`,
  and `tail` is empty for every line `Cut::End` lays out.
- `IconTile::with_name_cut(cut)` asks for that cut; `render::name_cut` keeps
  whatever ends the name — the extension after its last dot, or the file type
  after its last comma — from the one `Ending` parse the registry classifies
  with. Every other tile and text block keeps the end cut.

## FI17 — click a selected name to rename it

- A primary press on the **name** of the entry that was already the one
  selected entry — the grid tile's drawn name lines, the list row's name cell —
  that is released without travelling past the drag slop arms a rename. It
  opens once the desktop's double-click interval has passed since the release:
  the first moment the click can no longer pair into an activation. A second
  press inside the interval is a double-click and activates.
- Anything else disarms it: another press, a key, a scroll, a drag, a menu or
  overlay opening, a navigation or a listing change that moves the entry, and
  the window losing the keyboard. A press on a window that did not hold the
  keyboard selects and never arms, so the click that brings a window forward
  does not start editing.
- The wait is a deadline folded into the loop's one parked wait beside the
  marquee's step (`gesture::RenameArm`); there is no periodic timer.
- A listing change carries an armed click with its entry
  (`RenameArm::follow`), and a replaced listing lets it go. The rename opens
  only on the entry still chosen alone, with nothing holding the listing.
- The keyboard is tracked from the focus reports (`gesture::Keyboard`): the
  session reports a window coming forward and then delivers the press that
  brought it, so a press straight after a gain never arms. It reports every
  move of the keyboard, not only a press's — a raise from the icon bar, a
  popup, a closed window — reconciling what the apps were told with where the
  keyboard rests after each routed seat outcome and each served request, so a
  window that lost it knows before its next press.
- A due rename is checked on every turn of the loop, the turn that serves a
  running copy or delete included, and its opening repaints the entry and its
  field, not the window.
- Every way a rename opens — `F2`, the menu, New ▸, a click — selects the name's
  **stem**, everything before its extension (`rename_selection`), so typing
  replaces the name and keeps the extension. A name with no extension and a
  folder are selected whole; a bundle selects its stem and keeps `.app`.
- While the field is open a press inside it reaches the field (caret, drag
  selection). A press outside it commits: a refused name keeps the field open
  with its reason and the press is spent; an unchanged name closes the field
  and the press then acts as it would have; an accepted one closes it and the
  press is spent too, since the folder may have re-sorted under the pointer
  (`gesture::RenameCommit`).

## FI18 — a folder fans out what it holds

- `FolderSample` holds up to three `SampleCard`s, each a member's kind or a
  member **picture** (its `Thumbnail` key: path, identity, size, modification
  time, content generation, reading). The probe keeps what each record of its
  batch already carries, so no member is opened or statted to choose it. A
  picture card is a regular file with an identity whose type has a reading,
  exactly as `entry_icon` decides a tile's thumbnail.
- **Variety first, then fill.** One card for each of the most frequent families
  in turn; any cards left go round the families again, in the same order, while
  a family has members not yet shown. Within a family, members come in the
  batch's order. A folder of photos shows three of its photos; of text files,
  three text cards; of photos and one PDF, two photos and the PDF; of one file,
  one card.
- The picture is the folder's back, the cards fanned — each turned a few degrees
  about its foot so they rise out of the mouth as a spread — and the front. A
  picture card is the member's thumbnail on a white print with a fixed-ink edge;
  any other card is its kind's artwork, or its glyph on paper. A member that
  will not decode draws its kind's card instead; only a back or front that will
  not draw degrades the picture to the plain filled folder.
- `Surface::blit_transformed` (`lib/raster`) is the one transformed blit: an
  inverse-mapped bilinear sample of premultiplied pixels, clipped to the
  destination, whose edges anti-alias against transparency.
- A sample holding a picture reads member files, so it is thumbnail-class work:
  queued apart, served after icons and probes, swept when nothing asks. Its
  request carries the same sample with every picture card as its kind as the
  next tier, and a thumbnail-class tier still being produced does not stop the
  walk: the tile draws the tier below until the pictures land. The inline
  resolver declines thumbnail-class keys, so a program with no worker draws the
  kinds.
- A card thumbnail comes from FI25's store when the member's tile-side picture
  is held there (resampled to the card), and otherwise is decoded at the tile
  side, stored, and resampled — so opening the folder later costs no decode.
- One folder is one worker job, so it reads no more than one thumbnail may: a
  member larger than a third of `MAX_THUMBNAIL_BYTES`
  (`FolderSample::MAX_CARD_BYTES`) is drawn as its kind, and a document opened
  meanwhile waits behind no more than a single picture would make it.
- The cards are shared (`Arc<[SampleCard]>`): the listing entry, every cache key
  naming the folder and its kinds-only view hold one copy, so a key costs no
  allocation per frame, and a sample's equality is over its cards as drawn, so
  a kinds-only view is the same picture as any sample drawing those kinds.
- A cache key holding member paths is charged for them: an entry's metadata
  charge is its key's real heap size beside the fixed bookkeeping.

## FI19 — the marquee keeps up

What made the band lag was the loop, not the sweep: every queued motion sample
cost a whole paint and a blocking present, a burst left a backlog that stayed,
and each paint redrew everything under the band because its edge damage was
merged into the box spanning it.

- The manager applies every queued event before it paints, up to one mailbox's
  worth so a flood cannot hold the frame off. Each round's exact damage folds
  into its window's `tairix_window::Owed` account — clipped to the window and
  merged by least growth, so it stays bounded across any burst — and each
  window that owes is painted and presented once a turn. The present is
  synchronous, so it paces the window: input that arrives while one is in
  flight is applied together by the next turn, and a burst costs one frame.
  The turn that serves a running copy or delete drains the same way.
- A turn paints each owed rectangle under its own clip, and the grid and list
  skip, before composing it, every entry whose cell the clip excludes; the
  rail, the toolbar and the scrollbar are skipped likewise for a rectangle
  that misses them, so a part inside the listing composes only the listing.
  A growing band costs its moving edges, not its area.
- `WindowRequest::Present` carries a `DamageList`, the display protocol's one
  definition: the frame encode, the session's decode and the compositor's
  damage are each the rectangles rather than the box spanning them. A round
  reporting more than `MAX_DAMAGE_RECTS` merges the pair whose union grows
  least until the list fits, and any two that overlap merge, so a list's
  rectangles are disjoint: one naming overlapping rectangles is refused at
  decode, and the session checks every rectangle before converting any, so a
  present converts each pixel at most once and a refused one writes nothing.

## FI20 — ground between tiles

- A tile's **body** is its picture and its name's drawn lines, each widened by a
  half inset (`TileLayout::body_rect`): the press target, the hover and
  selection plate, the drop-target outline, and what a marquee has to touch.
  The rest of the cell is ground, so a press there clears the selection and
  starts a band exactly as between cells, and a name drawn on one line leaves
  the second line's space as ground too.
- A hit-test stays arithmetic to the cell and then tests that one entry's body.
  A band selects an entry whose body it touches: a cell whose picture it touches
  is certain, a cell it misses entirely is not, and only the cells along its
  edges are tested by body, so a sweep still visits only the entries entering
  or leaving it.
- Bodies never meet: the cell keeps an inset either side of the widest body,
  and the grid keeps its gap between cells, so two half insets and a gap of
  ground lie between tiles side by side, and at least a half inset and a gap
  between rows, however the window is sized.
- The desktop's icon field takes the same body.

## FI21 — Select All and Clear Selection

The context menu gains **Select All** (`Ctrl+A`) and **Clear Selection**
(`Ctrl+Shift+A`) as a group after Paste. Select All is enabled while some entry
is unselected; Clear Selection while anything is selected. Both accelerators
work wherever the listing holds the keyboard, and the repaint is the entries
whose mark changed.

## FI22 — a thumbnail's frame

A thumbnail is drawn with a one-pixel frame in the theme's `frame` role, so a
picture whose edge matches the window's ground still reads as a picture. The
frame follows the picture's own bounds, not the square it was centred in: the
sandbox reply states the fitted rectangle, the host refuses one outside the
square or empty, and the cache keeps it beside the pixels. The frame is drawn
at paint time, so it follows a theme change; it lies on the picture's outermost
pixels, so nothing is drawn outside the slot.

## FI23 — icons cast shadows

Every picture a grid tile draws — artwork, thumbnail, folder fanout, or glyph —
casts a small soft shadow: its own coverage dropped below it and softened, in
the theme's `drop_shadow` colour, reaching `icon_shadow_reach`
(`IconTile::shadow_cast`). `tairix_raster::cast_shadow` is the one recipe for
a shadow cast from a picture's coverage (the pointer's shadow is built from it
too). The mask is cast once per (picture, side, cast) and retained as its own
artwork-cache entry (`IconArtwork::shadowed`), so a frame blits a mask and
never blurs; a tile asked to cast one (`IconTile::with_picture_shadow`) and
handed no retained mask casts it from the picture it draws, so a cached and an
uncached picture draw alike. Only a cast mask is kept: a shadow that cannot be
cast or kept — memory short — is withheld for that draw
(`IconPicture::shadow_withheld`), which the tile draws as no shadow rather
than blurring itself, and asked for again on the next. The reach is less than the tile's inset, so a
shadow never paints outside the cell and damage stays per cell. The greeter's
account tiles and every other control draw none.

## FI24 — the content generation

- ARXFS gives every file and link a **content generation**: a new one with every
  change to its data — creation, a write, a truncate or extend — and nothing else
  moves it. Metadata (mode, owner, times, attributes, a rename, a second name)
  leaves it alone, and no caller can set it.
- It is drawn from one **volume-wide sequence** the transaction root carries,
  not counted per inode: a crash can roll an inode back, and an inode number
  handed out in a lost transaction can be handed out again, but a generation is
  never repeated, so (volume, inode, generation) names one version of one file's
  content.
- The first data change of a mount advances the sequence by a stride (2³²) past
  the committed value, commits, and flushes the slot before its operation
  returns. Calls are exclusive, so no reader sees a generation of the mount
  before the stride is on the medium; what a lost transaction or a fall-back to
  an older ring slot can have handed out is bounded by the dirty-age window and
  write-back cap, far below the stride. A mount that changes no file's data
  writes nothing for it, and a write of no bytes or a truncate to the current
  size changes no data.
- It is minted by the call that changes the data, in the transaction that
  persists the change, so a reader that sees the same generation before and
  after reading read one version.
- `NodeInfo::content_gen`, `FileStat::content_gen` and `DirEntry::content_gen`
  report it (ARXFS format version 3); `0` means the volume keeps none (ext4,
  FAT32, ADFS, covered mount points), and a consumer then treats no value as
  exact.

## FI25 — thumbnails that persist

- **Exact or not at all.** Only a file whose listing reports a content
  generation is stored; anything else decodes as before and lives in memory
  only. A stored picture is keyed by (volume, inode, content generation,
  reading), so a changed file is a different key and a stale picture cannot be
  served.
- **One side.** A blob holds pictures of one side: the grid tile's picture,
  the only side the file manager draws a thumbnail at, and the side every
  folder card is decoded at before it is resampled. It is laid out for the side
  of a run's first thumbnail; after a scale change within a run, pictures
  decode into memory as before, and the next run lays the blob out afresh at
  the new side, since none it held fits.
- **Nothing browsed is written.** The store is one blob in the file manager's
  own bulk store (`Library/Apps/os.tairix.files/Blobs/thumbnails`), reachable
  only by this application through the app-data service; another program, and
  the account's own shell, cannot read or plant a picture in it.
- **A hit is re-proven.** Serving a stored picture opens the file under the
  user's own identity without following a link and compares its stamp —
  identity, size, modification time, generation — with the key, exactly as a
  fresh decode does, so a file the user can no longer open is never pictured.
  No content is read.
- **A fresh decode is stored only if it read one version**: the generation is
  read again after the upload, and a decode that raced a write is drawn but not
  kept.
- **The decoder revision** the pictures were drawn by
  (`tairix_sandbox::imagerender::THUMBNAIL_REVISION`) is in the header, so a
  decoder or fitting change forgets every picture an earlier one drew, and a
  blob shorter than its layout — another instance cut short mid-format — is
  laid out afresh rather than adopted. A lookup reads into a fallible buffer:
  short of memory, it misses.
- **The format** is a header naming the side and slot count, a table of slot
  headers, and the slots: a key hashes to a four-way set (a fixed seed, so the
  next run finds it), a lookup reads that set's four headers and then one
  payload, and an insert takes the key's own slot, an empty one, or the set's
  oldest by insertion — a hit writes nothing. A slot's header carries a
  checksum over itself and its payload and goes down after the payload, so a
  torn write reads as an empty slot. The file is sized to the blob's extent
  ceiling, written sparsely, and needs no scan at start; a header for another
  side or version reformats it.
- **No lock.** The store lives on the reader thread alone. A second instance
  opens the same blob — a delegated descriptor takes no advisory lock — and
  the slots are what make that safe: two instances writing one slot leave a
  header and a payload that do not check out, which reads as empty, never as
  the wrong picture.

## Sequencing

FI1–FI3 are the type and tile change. FI7 precedes FI8 and FI9 (both build on
the drawn multi-selection). FI5 precedes FI4. FI6 is independent. FI13 precedes
FI11 and FI14 precedes FI12; FI12 builds on FI11's artwork-desk work.
FI16 precedes FI20 (a body is measured from the laid-out name). FI24 precedes
FI25, and FI25 precedes FI18, whose card thumbnails come from the store.

## Not in this plan

Dragging from the desktop into a window, dropping onto application windows
other than the manager's own, and spring-loaded folders.
