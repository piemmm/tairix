# `tairix-machines`

The machine library both emulators keep, `PCEmulator.app` and
`OpenMicro.app`: the record of every machine, each machine's last screen and
snapshot, the library window of machine cards, how a machine stands, the
suspends that hold a window open until they are kept, the new-machine
assistant and a card's menu. An application implements `record::Kept` for its
own machine and gives its models, its blob names and its bounds; everything
else is this crate's, so the two emulators keep their machines alike.

The model is host-tested and `no_std`. The `rt` feature adds the runtime
halves a freestanding application links: `store`, over the application's own
store, and `window`, over the window channel.

| Item | What it is |
|---|---|
| `record::Kept` | An application's machine as the library keeps it: its number and name, its own fields' encoding, and the blobs it owns beside its state. |
| `record::Library` | The machines: numbers minted once and never reused, a fresh name for a new machine or a copy, at most `Kept::MAX_MACHINES`. |
| `record::Record`, `Loaded`, `Found` | The record written to `library-a` and `library-b` in turn, each closed by a CRC and a generation; the newer whole copy is read, so no write can leave the library torn, and when neither reads it reads as empty and sweeps nothing. |
| `record::Writes`, `Landed` | One record write at a time: a change made while one is under way is written once it lands, and a deleted machine's blobs go only once a record without it is kept. |
| `state` | A machine's state blob: its last screen, compressed, and its snapshot while it is suspended, each checked on its own and bounded by the application's largest frame and snapshot (`Bounds`). |
| `standing::Standing` | Off, running, being kept as it suspends, or suspended: what may be started, copied, deleted or refitted. |
| `keeping::Suspends` | The machines suspended whose state is not yet kept, each holding its window open until it is, so closing a window never loses a machine. |
| `shelf::Shelf` | The library window's model: a card of each machine's last screen and name, the commands *New*, *Start*, *Settings*, *Duplicate* and *Delete* above them, the band beneath saying what the chosen machine is; double-click and Return start, Delete deletes, the arrows choose, a secondary press asks for the card's menu. One layout serves the paint and the hit test. |
| `shelf::LibraryLook` | The library window's title and the size it opens at. |
| `shelf::Pictures` | Each machine's last screen drawn once at the cards' size. |
| `assistant::Assistant` | The *New Machine* dialog over an application's models: a model, a name, and the model's one further choice. |
| `menu::card_menu`, `card_command` | A card's menu — Start (Resume, Show Window), Settings, Duplicate, Delete — and, while the machine runs, its own rows. |
| `menu::bar_rows`, `LIBRARY_ROW`, `BAR_CLICK` | The icon-bar menu's own row, *Machine Library*, between the information row and *Quit*, and the slot's click that opens the library. |
| `menu::Followed` | A menu kept up, restated only when what it shows changes. |
| `menu::Effect` | The tube's effects a machine window's *Display* submenu turns on and off. |
| `store` (`rt`) | The library read, swept and drawn (`load`), a record or a state kept, a machine's blobs removed or copied, and a resume's snapshot read (`resumable`), all over the application's own store and off the loop. |
| `window` (`rt`) | `LibraryWindow` and `SheetWindow`, the library window and an owner-modal dialog over it, each painted into a surface of its own and presented once a turn; `declare_bar` and `bar_ask`, the icon-bar presence; `delete_question`, the question a deletion asks first. |

## Bounds

An application's store holds at most `APPDATA_BLOB_MAX_COUNT` blobs: the
record's two copies, then a state blob per machine and the parts each machine
owns beside it, so an application's `MAX_MACHINES` must fit what
`record::store_holds` answers for its parts, and what it keeps of its own
besides. A name is at
most a window title's bound. A state blob is refused past the bounds its
application gives, before anything is decoded.

## Testing

Each module has its own tests; `tests/fuzz_library.rs` feeds the record and
state decoders hostile bytes under `cargo xtask fuzz`.
