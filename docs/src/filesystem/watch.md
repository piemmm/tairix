# Directory watches

A program showing a directory — the file manager, the desktop's pinboard, the
file picker — follows every change any program makes to it: a document saved
from an editor, `echo moo > notes.txt` in a shell, a build writing thousands of
objects. It neither polls nor re-reads the directory to find out.

## What the kernel records

Every mounted volume's change table (`kernel/core/src/fswatch.rs`,
`VolumeWatch`) is claimed from the kernel's `WatchRegistry` by the cached
volume layer every mutation of that volume passes through
(`kernel/core/src/fs/changelog.rs`): create, link, symbolic link, unlink, both
ends of a rename, write, truncate, a mode, owner or ACL change, and an
extended-attribute change. Who made the change does not matter, so an
application, a shell redirection and the account-administration engine are all
seen. A mutation the driver refuses records nothing.

- **One feeder per volume.** A volume that detaches and returns is fed through
  the table it left, so its watches survive the remount. A second volume
  presenting a mounted volume's identity gets no table and cannot be watched;
  one whose names now match differently retires its old table, ending every
  watch on it. A table nothing feeds or holds is dropped.
- **Names, not contents.** A watched directory keeps a journal of the *names*
  that changed, coalesced: a name written a thousand times is one entry. What
  a name now is gets answered when the journal is drained, by resolving it
  again under the drainer's own authority and through the checks `readdir`
  applies, so a watch discloses nothing a listing of the same directory would
  not. Names are decrypted user data, so each copy the watch path holds is
  wiped before it is freed.
- **Subfolders report on their own entry.** A change inside a subfolder is
  recorded against the subfolder's entry in its parent, attributed through a
  short trail of the volume's recent lookups, so a parent listing's folder
  stays current. A metadata change is attributed to the name it was made
  through, and a new hard link reports both its own name and the one it was
  made from, whose link count moved. The trail holds the sixteen bindings
  resolved most recently, so a folder reached through more distinct
  components than that since its own was resolved loses only that one report
  of its entry in its parent; the folder's own listing never depends on it.
- **A file's other names are not reported.** A change made through one name
  of a file with several hard links reports that name alone; its other names,
  in this directory or another, show their old size and stamp until listed
  again.
- **Bounded, fairly, and honest about it.** Every journal together holds at
  most a 512th of the machine's memory, and each an equal share of that, never
  past 32 KiB. A journal past its share gives up its oldest names, and only a
  watcher still owed one rescans; arming a new journal holds every other to the
  smaller share at once, so watches that never drain crowd out no one else's.
  Memory pressure shrinks the budget as it does every record of filesystem
  metadata (`ReclaimClass::FsMetadata`): kept through *mild*, cut to its low
  mark at *moderate*, emptied at *severe*, since a rescan is the dearer read. A
  volume that folds case (FAT, ADFS) always rescans. A journal drained empty
  returns its memory. Nothing is ever dropped silently.

### What a drain answers

Each drain resolves the descriptor's path again under the drainer's authority:

- **gone** — the path no longer reaches the watched directory, names no
  directory, or may no longer be listed, or the volume's table was retired.
  The watch is spent: it reports nothing more, refuses to join a wait-set, and
  counts against the limit until its descriptor closes.
- **rescan** — the journal could not keep up (above), a directory's mode,
  owner or ACL changed since the last drain (the journal may hold names the
  drainer could not have listed), a mount appeared or left beneath the
  directory, or reading an entry faulted.
- **changes** — otherwise: each name with what it now lists as, or absent.

## The interface

| No. | Call | Arguments | Returns |
|---|---|---|---|
| 136 | `fs_watch` | `u32 fd`, `u64 latency_ns` | `errno` |
| 137 | `fs_watch_read` | `u32 fd`, `user_ptr` (buf), `len` | `u64` (bytes) |

Both are gated on `CAP_FS_ACCESS` and unaudited, like `fs_readdir`.

- **`fs_watch`** arms a watch on an open directory descriptor, once
  (`AlreadyExists` after). The watch belongs to the open file description and
  ends when it closes; there is no watch-descriptor namespace to leak. Each
  process holds at most its `dir-watches` limit of them — by default
  `max(64, RAM / 2 MiB)`, settable with `ulimit` — and `latency_ns` is at most
  `DIR_WATCH_LATENCY_MAX_NS` (60 s). A directory on a volume with no change
  table refuses with `NotImplemented`.
- **`fs_watch_read`** drains into a `DirChangeBatch` (`lib/abi/src/fs.rs`): an
  8-byte header — status (`Changes`, `Rescan`, `Gone`), a *more* flag and a
  record count — then `DirChange` records, each either *present* (tag 0 and the
  very `DirEntry` record `fs_readdir` emits) or *absent* (tag 1, a `u16`
  length, the name). The buffer must hold at least `DirChangeBatch::MIN_BUFFER`
  bytes. A batch is committed only once it has been copied out, so a faulting
  buffer loses nothing.
- **The `DirWatch` wait-set member** (kind 13; `id` is the descriptor) is how
  a consumer waits. It is readied by the journal advancing, by the table
  retiring, and by any epoch the path last resolved against moving: the mount
  table's, the path epoch (any directory or symbolic link renamed, removed or
  replaced) and the access epoch (any directory's mode, owner or ACL changed).
  Those two are the whole machine's, because a path's ancestors need not be on
  its directory's volume; a directory's first owner, given to it as it is made,
  is no change of anyone's access and moves neither. Both are read before the
  path resolves, at arming and at every drain, so a move while it resolves
  readies the member at once. A reported move is reported once and the member
  then holds until its watcher drains, since until that drain re-resolves the
  path the directory may not be the watcher's to follow; a watcher that stops
  draining learns nothing more about it. It is edge-triggered and paced in the
  kernel: an isolated change reports at once, and a storm at most once per
  `latency_ns`, the trailing report armed as the wait's one-shot deadline
  rather than a timer tick. A change no member waits on wakes no one, and a
  run of moves wakes a parked member once. Pacing is the member's own, so a
  description joined to several wait-sets is paced in each, while the journal
  it drains is the description's: whichever drains first takes the batch.

### Arm, then list

A consumer opens the directory, arms the watch, and lists through the same
descriptor. A change between the two is already in the journal, so nothing
falls between the listing and the first report (`WatchedDirectory::list` in
`lib/browse`).

## Consumers

`lib/browse` holds the one implementation every directory view shares
(`lib/browse/src/watch.rs`): the arm-then-list read, the drain, the
`WatchedDirectory` that joins its consumer's wait-set once and withdraws that
member before its descriptor can close, the `Watches` desk, and
`merge_changes`, which folds a report into a sorted listing. One pass over the
listing finds every name the report touches — a filter on each name's length
and a Bloom filter pass over almost every entry without searching the report,
whose own hash needs no key because a name crafted past it costs only that
search — and an entry that keeps its place is replaced where it stands; what
goes, arrives or moves is merged so the listing is moved at most once. Its
cost is O(n log k) for n entries and k changes, against O(n·k) for one search
per change, and everything it needs is allocated before the listing is
touched: without the memory, the listing is left as it was and the folder is
read again. The desk commits a consumer to the watch armed with the
listing it takes, drops the watch of a read whose answer was superseded, and
reads a reload of the same folder through the watch already held, so a reload
neither re-arms nor restarts its pacing. The file manager, the desktop's
pinboard and the file picker each:

- drain on their reader worker, never on the event loop — and where no worker
  is granted, read and drain on the loop itself (`Watches::read_here`,
  `Watches::drain_here`), slower but still following the folder;
- merge in place, keeping the focus and selection on the entries they named,
  and re-list instead when the merge could not have its memory;
- repaint only the rows or icon cells the merge moved;
- hold a report back while something holds the listing — in the file manager
  a menu, an inline rename or a drag; the pinboard and the picker hold none,
  and carry their selection, hover and pending double-click through the merge;
- on **gone**, let the watch go and read the folder again — the file manager
  and the picker climb to the nearest ancestor that can still be read;
- after a change they made themselves, read a folder again only when its
  listing does not follow it (`DirectorySource::follows`, `Watches::follows`):
  a followed folder reports the change like any other.

Desktop listings arm with a 200 ms latency (`WATCH_LATENCY_NS`): a lone change
shows at once, and a storm repaints a window at most five times a second. A
consumer holding more than `PENDING_CHANGES_MAX` changes collapses them to a
rescan. A folder on a volume with no change table does not refresh by itself:
its view's Refresh command reads it again, and any other refused arm is stated
once on `stderr`.

## Compared with inotify and FSEvents

A watch is a property of an open description, so it cannot outlive or be
confused with the directory it was armed on. Its journal coalesces by name
and degrades to a rescan by design instead of overflowing a queue, storms are
paced in the kernel rather than delivered event by event, and the state of
each changed name is answered under the reader's own authority at the moment
it reads — a watcher that loses the right to list the directory learns it is
gone rather than reading on.
