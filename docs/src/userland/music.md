# Music (`music.app`)

The desktop player (`plans/SOUND.md` SND14): one window, one playlist, played
gapless by `tairix-player`'s engine. Its keys are its own Help document; this
page is how it is built.

## No filesystem capability

The player opens nothing itself. A file reaches it as a one-shot delegation:
from the session's trusted picker — a file, or a folder, whose files of a type
the player's signed manifest associates the session delegates one by one
(`PickPurpose::Folder`, at most `WINDOW_FOLDER_PICK_MAX` of them) — or handed
over by the file manager through the desktop's single-instance funnel. Each
descriptor is shared by the window, the worker and the playback thread until
the last lets it go.

No byte of a file is parsed in the player. Its sound is decoded in the audio
sandbox (`lib/sandbox::audiodecode`) and its album art — the picture its
`Metadata::cover` range names — in the image sandbox, each a capability-empty
worker the player's own binary is re-entered as.

## Three threads

* **The window** owns the player's state (`tairix_music::view::Player`) and
  never waits on anything but its own wait-set. Input changes state and reports
  the rectangles it changed; a paint reads only state.
* **Playback** is the engine on a thread of its own, parked on the decoder's
  pipes, the stream's mailbox and the window's orders. It posts its status and
  wakes the window. A level the window posts displaces any not yet carried out,
  so a dragged volume costs the stream one call however fast it moves.
* **The worker** reads each track's tags, decodes its cover, lists the output
  devices and saves the listener's settings. A job the worker has no room for
  waits in order on the window's side; none is dropped.

## Continuous controls act where they settle

The seek slider moves the position shown as it is dragged and seeks once, on
release. The volume slider sets the stream's level as it moves and saves it, on
release, through the worker. The meters repaint their own rectangle and
nothing else; a change that alters nothing reports no damage.

## The playlist

Both threads hold a copy of the playlist (`tairix_music::Playlist`), changed by
the same closed set of edits, each carrying everything it draws on — a shuffle
carries its seed — so the copies stay alike. A shuffle is stable: each entry's
place is a draw from the seed and its own name, so adding or removing one moves
no other. Repeat plays the list again or holds the track; *next* still moves on
under the latter.

## Volume

The volume slider is the application's level, kept for the listener in its own
settings. Each track's own level is its Replay Gain track gain, applied by the
engine when *Level tracks by their own loudness* is on (the default), so tracks
mastered at different levels play at one loudness without a library database.

## Settings

The volume, shuffle, repeat, loudness and chosen output are kept in the
player's app-data settings: read once at start-up, before the window opens,
and written by the worker. The output is kept by its location, its `audio:`
reference across boots, never by the audio service's id for it, which is one
boot's. An output that is no longer connected falls back to the default with
that stated.

## Testing

The engine is host-tested: the playlist (order, stable shuffle, repeat,
removal answering what followed), the layout at three scales (every part
inside the window, none overlapping, the transport keeping its room as the
window shrinks), the player's state (adding to a stopped player starts it,
one seek per drag, the volume saved once, keys, menus, settings), and the
damage each change owes (the meters alone for a level change, nothing for a
change that alters nothing, a paint scoped to one part changing no pixel
outside it).
