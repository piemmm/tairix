# Screen capture

The desktop session is the only thing on TAIRiX that captures the screen. No
application can ask for a capture: one starts only from the user's own chord
or a click on the desktop's capture bar, and the compositor's capture tap
composes it from the session's own scene (`plans/CAPTURE.md`).

## Shortcuts

| Keys | What it does |
|---|---|
| `Meta+Shift+3` | Saves a picture of the whole screen. |
| `Meta+Shift+4` | Veils the screen to drag out an area. `Space` switches to clicking a window; `Alt`-click leaves its shadow out. |
| `Meta+Shift+5` | Opens the capture bar. |

Adding `Ctrl` puts the still on the clipboard. A capture shortcut pressed
during a recording stops it, and once it is stopped the shortcuts capture
again while its file is finished; one pressed while the bar, the overlay or
the countdown is up cancels it, as `Escape` does. The keys are matched by where
they sit on the keyboard, before any seat holder sees them, and never while
the lock, the authorisation sheet or the screensaver holds the keys.

## The capture bar

The bar sits at the bottom centre of the work area as floating chrome: close,
the three stills (screen, window, area), the three recordings, *Options*, and
*Capture* or *Record*. Each part's tip comes through the seat's one tooltip.

*Options* opens the seat's menu chain above the bar:

| Option | Choices | Setting |
|---|---|---|
| Save to | Desktop, Pictures (Videos for a recording), Clipboard | `capture.save_to` |
| Timer | none, 5 or 10 seconds | `capture.timer_s` |
| Duration | unlimited, 10 or 30 seconds, 1, 5 or 10 minutes | `capture.duration_s` |
| Sound | none, microphone, desktop sound, both | `capture.sound` |
| Show Pointer, Show Clicks, Show Floating Thumbnail | on or off | `capture.pointer`, `capture.clicks`, `capture.thumbnail` |

Each choice is written to the user's desktop settings once, and stands while
it is written; a refused write puts the stored choice back. A recording is
never put on the clipboard, so that choice keeps it on the Desktop. The
bar's captures run the timer as a countdown on a plate; a countdown is a park
deadline, and `Escape` discards it.

## What a capture shows

- The area asked for, with the capture's own chrome — the bar, the overlay,
  the countdown and the thumbnail — left out; the thumbnail is taken down as a
  recording starts. An application's own tooltip is part of the screen and is
  shown.
- A window capture shows that window and its transients alone, with its
  shadow unless `Alt` was held.
- The lock and the authorisation sheet are shown black, and locking ends a
  recording.
- The pointer, when *Show Pointer* is on; *Show Clicks* rings it at each
  press while recording.

The composed picture, the PNG written from it and the clipboard's copy are
wiped as they are let go; the floating thumbnail's small copy and the PNG
encoder's working memory are not. Every still is logged
(`CAPTURE_TAKEN`, 20037), as is every capture refused (`CAPTURE_REFUSED`,
20040), every recording's start and end (`RECORDING_STARTED`, 20038;
`RECORDING_ENDED`, 20039), and every capture given its name in its folder
(`CAPTURE_KEPT`, 20042), its `how` saying whether it was a still, a
recording, or what a session ended mid-capture left, named at the next
start.

The `tairix-test-capture-qemu-aarch64` vertical boots the desktop on four
cores with such a leftover in the account's pictures. Once the revealed
desktop's icons are drawn as they will stay (`PINBOARD_SETTLED`) it dumps the
screen and presses `Meta+Shift+3` at the seat keyboard; once the still's icon
has settled among them it opens the capture bar, records the screen from it,
dumps the screen being recorded and stops the recording with the chord. It
passes once the leftover, the still and the recording are all kept, the
recording having lost nothing; the host then reads the still back off the disk
and holds it to the first dump, every pixel off the bar the same, and decodes
the recording's last frame and holds it to the second to within the codec's
loss, the frame coded across the encoding pool's threads. The bar is left
out: its clock, Switchboard readings and icons settle on their own after the
reveal. Its root volume is larger than other verticals', to hold a
screen-sized still and recording.

## Stills

A still is composed into a buffer the heap may refuse and kept off the serve
loop by the session's capture file worker as a PNG recording 96 dpi at the UI
scale:
`Screenshot YYYY-MM-DD at HH.MM.SS.png` in the chosen folder, numbered ` 2` to
` 64` where the name is taken, written to a hidden scratch file of its own
(`.keeping.png`, `.keeping 2.png`, …, made afresh so no two captures share one)
and renamed without replacing. While the clock is not set the date is left out,
and a name too long for the filesystem with its date and number is refused
before anything is written. The screensaver's kept pictures are named by the
same rule.

On the clipboard a still is a `ClipboardKind::Png`. One past the clipboard's
16 MiB bound is saved to the chosen folder instead, and a notice says so. The
floating thumbnail shows for five seconds at the bottom right of the work
area, made from the picture the session already holds.

## Recordings

The session starts `recorder.app` over two pipes only it holds and speaks
the protocol in `tairix_abi::recorder_ipc`. The kernel attests the recorder's
process as the session starts it, and the session grants that process the
output file and two slots of the session's own memory, whatever the recorder
says. Each slot's grant resolves for that process alone, and the recorder
maps it only as a grant of the session named as its one operand — by the
session as it starts the recorder, or by the broker for an administrator's
recording — since its pipes attest no peer. The slots are made once the
recorder says it is up; a slot that cannot be made or granted ends the
attempt with its reason, and they are released however the recording ends. Frames go at most thirty a second and only when something changed.
While both slots are busy, or the last frame is still queued, damage merges
into the next frame. What the recorder does with each frame and with the
sound is [`recorder.app`](../userland/recorder.md).

A recording with sound first asks the elevation broker, off the serve loop,
whether the signed-in account may allow `recorder.app` itself — the
`Authorisers` question the authorisation prompt asks. One that may (its
ceiling carries `CAP_AUDIO_CAPTURE`) records as above. One that may not is put
to the system's authorisation sheet: an administrator's password runs the
recorder **as that administrator, for that one recording**. The login broker
starts it in this session over pipes it then hands the session
(`ElevateRequest::LaunchWired`), and the session speaks the same protocol over
them, its file and its slots going to the process the broker attested. A
cancelled sheet starts nothing and says so. The sound choices carry no
Authority Mark: a choice an administrator can allow is not one that cannot be
taken. Until the sheet is answered nothing is recorded and no indicator
shows, and while it holds the keys the capture shortcuts are not matched.

A recording is kept as `Screen Recording YYYY-MM-DD at HH.MM.SS.webm` on the
Desktop or in `Home/Videos`. A shortcut, the indicator in the notification
area, or the duration stops it; the lock or a change of user finishes it;
no frames are taken while the screensaver is up, and a logout waits for the
recorder to finish its file. What changed since the last frame goes with the
end. A recorder that dies, a full disk or a refused start is stated in a notice
— one whose program never loaded says why — and what was written is kept. A
recording cut short by the session's end is named at the next start; an empty
one, from a recorder that died before it wrote, is removed.
