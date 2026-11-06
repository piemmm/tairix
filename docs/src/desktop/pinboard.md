# The desktop pinboard

The **pinboard** is the desktop backdrop: the wallpaper drawn behind
everything, the icons for the logged-in user's `Desktop` folder drawn over
it, and the menu that appears when the backdrop is right-clicked.
The binding design is `plans/PINBOARD.md`; this page is the reference for
how it is put together and where each decision lives.

The pinboard is a *layer*, not a window. The compositor
(`AGENTS.md` §10, [the window manager](./wm.md)) keeps one desktop layer
beneath every window, and the session paints the pinboard into it. A
wallpapered desktop therefore costs the compositor nothing extra to
*composite*: it blends the same single surface it always did, whatever the
picture.

What it does cost is *repainting* that layer, and only there. The desktop is
the bottom of the stack, so marking all of it recomposites every window above
it and throws away every frosted backdrop over it — on a 1080p screen, most of
a megapixel of blur to move one highlight. So the model reports the icons
a gesture actually changed and the session repaints only those
(`DesktopShell::present_desktop_area`); the whole layer is repainted only when
the whole layer changed — bring-up, a new wallpaper, a theme switch, or adopted
settings that restyle or re-lay it (`PinboardChange::layer`). A listing is not one of those: a change the folder's watch reports is
merged in place (`Desktop::apply_changes`), and a fresh listing — a rescan, a
folder read again, a name the desktop made in a folder its listing does not
follow (`Desktop::follows`) — is adopted whole (`Desktop::relist_into`,
`Desktop::resume_into`); either way the icons whose
place or highlight moved are reported, since each icon is a rectangle of its
own and one that stays where it stood costs nothing. Icon artwork arriving from the
decode desk is not one of those: a landed decode can only change the picture
inside the tiles that draw through it, so `Desktop::mark_artwork` reports those
icons and a batch that pictures nothing shown costs no frame at all. See
[the session's desktop layer](./session.md#the-desktop-layer-wallpaper-or-backdrop-then-icons).

## The pieces

| where | what it owns |
|---|---|
| [`lib/wallpaper`](../lib/wallpaper.md) | the settings document, the shipped wallpaper catalog, and the placement geometry |
| [`lib/image`](../lib/image.md) | decoding a wallpaper (PNG and JPEG), including reduced-scale decode |
| `lib/raster` | the one image resampler both the icon and wallpaper paths use |
| [`lib/sandbox`](../security/sandbox.md) | decoding and placing a wallpaper inside a capability-empty worker |
| `lib/browse` | the icon grid, its two arrangements, the shared sort, and the new-folder naming rule |
| `userland/gui/session` | the pinboard itself: the layer, the backdrop menu's row model, the settings, and the apply service |
| [`userland/apps/settings`](settings.md) | the Wallpaper pane the user actually clicks, over the session's two served requests |

Nothing about the pinboard lives in the kernel or in a driver.

## The settings document

One small document per user, in the desktop session's **published** app-data
scope ([the app-data client](../lib/appdata.md), `plans/APPDATA.md` §3.11).
It carries six keys: which wallpaper, how it is fitted, the backdrop colour
behind it, which corner the icons arrange from, how they are sorted, and
whether an icon put down settles onto the grid.
[The `lib/wallpaper` page](../lib/wallpaper.md) is the reference for the
registry, the defaults, and the bounds.

Three properties matter more than the format:

- **Absent is not broken.** A fresh account has published nothing, and the
  defaults apply silently. A value this build's registry does not accept
  leaves *that one setting* at its default **plus** a warning on `stderr` —
  the desktop comes up and says why, rather than guessing at a half-parsed
  intent or refusing to start. A store that cannot be read leaves every
  setting at its default, says so, and nothing is published over them until a
  read succeeds: a publish replaces the whole document, so one made over the
  defaults would erase the user's settings.
- **The session is the only writer, by construction.** An application
  publishes only its *own* scope, so no other program the user launches —
  including Settings — can write the desktop's document at all. The
  in-memory settings adopt an edit only *after* the publish succeeded, so
  what is on screen and what is stored cannot diverge.
- **Any application may read it**, by naming the session's bundle identifier
  on a request shape that carries no scope field — so Settings can show
  what is in effect without being able to reach anything else the session
  keeps. That replaces the hand-rolled `~/Settings/Pinboard/pinboard.conf`
  a settings surface used to open directly, a file every application of
  that user could also rewrite.

## Changing the settings

The backdrop menu and the Settings application both **ask**; the session
decides, applies, and persists. The rendezvous is
`PINBOARD_ENDPOINT`, a reserved, seat-scoped call endpoint in `lib/abi`,
bound like the notification and window rendezvous: the session that owns the
seat serves the pinboard shown on it, and nothing else may.

The request carries the **rendered settings document** rather than a struct
of discriminants. That is deliberate: a second encoding of the same model
beside the document's own grammar would be two definitions of one thing,
and the two would eventually disagree.

The session **merges** the request over what it currently holds rather than
replacing it. More than one surface asks the desktop to change and none of
them shows every setting: the backdrop menu and Settings' Wallpaper pane
edit the pinboard keys, its Appearance and Accessibility panes edit the
appearance keys. Each renders only the keys it edits, and a key a sender did
not name keeps the value the desktop has — so choosing a picture cannot
reimpose whatever appearance another pane happened to open on, and vice
versa.
Taking the absent keys as their *defaults* would do exactly that, on every
single apply. A document the registry refuses is refused whole: the merge
runs on a copy, so a refusal partway through leaves the desktop untouched.

The security posture is worth stating plainly, because it is easy to get
wrong:

- The session serves a request only from a caller whose kernel-attested
  origin carries the session's own uid. Anything else is refused and
  recorded on the audit trail (`PINBOARD_UNATTESTED`, naming both uids).
- The document is configuration data and **carries no authority**. It
  *names* a wallpaper path; the session then reads that path under its own
  identity. A caller therefore cannot use the pinboard to reach a file it
  could not read itself — the classic confused-deputy shape, closed by
  construction.

Reading is not brokered at all: a surface reads the user's own published
document directly, because a reader needs no coordination.

### The file manager's settings

`ApplyFiles { document }` is the desktop apply's twin for the file manager's
own registry (`tairix_browse::FilerPreferences`, every key under `files.`),
which the session keeps in the same document and publishes with its own: it is
merged over what the session holds, refused whole for a key outside that
registry or a value it does not accept, and answered once the publish has
landed. A publish that changes them moves `DesktopInfo::settings_generation`,
which is how the file manager learns to read the document again.

### The sound requests

The same rendezvous carries the Sound Effects pane's requests about the user's
sound choices (`plans/SOUND.md` SND23), attested exactly as an apply is:

| Request | Answer |
|---|---|
| `SoundRead` | the settings in effect, the user's over the machine's, and each event's file by its name alone (`encode_sound_reading`) |
| `SoundThemes { from }` | up to `SOUND_THEMES_PAGE` themes from the `from`th, in name order; a short page is the last (`encode_sound_themes`) |
| `SoundApply { document }` | the status, once the choices are stored; a document naming a file, or any setting of a sound no user's choices reach, is refused whole |
| `SoundPick { event }` | the status, once the session's own picker has closed, chosen or not; a file chosen turns the event on |
| `SoundUseTheme { event }` | the status, once stored, the event playing its theme's sound and turned on |

No request names a path: the session's picker is the only way a file is
chosen, so no caller can have the session play one the user did not pick.
Unlike the pinboard document, the choices are private, so a read *is*
brokered. Every frame is exactly as long as its operation's, and the
rendezvous answers at most `PINBOARD_MAX_REPLY` bytes.

## Drawing the wallpaper

A wallpaper is untrusted input — a shipped master no less than a file the
user picked — so it is never decoded in the session's address space. The
session reads the bytes under its own identity, bounded, and hands them to
the [parser sandbox](../security/sandbox.md), which decodes the image,
places it, and returns the finished pixels.

What makes this affordable:

- **Reduced-scale decode.** A JPEG decodes at the smallest DCT scale that
  still covers the screen, so a 1920×1080 desktop never materialises a 4K
  master's 8.3 million pixels to throw most of them away. PNG has no reduced
  scale and decodes whole.
- **Banding.** A screenful of RGBA exceeds the sandbox's fixed 8 MiB frame
  bound above 1080p. The bound is a defence and is not raised; the pixels
  are transported in bands instead.
- **Prepared once.** The session holds one prepared, screen-sized surface
  and re-prepares it only when the wallpaper, the fit, or the screen
  geometry changes. Nothing decodes, resamples, or parses on a frame path.
  The surface is held in the shared reclaimable-memory model, so a machine
  under pressure drops it and re-prepares on demand.
- **Prepared elsewhere.** The read and the sandbox round trip run on a worker
  thread that owns its **own** capability-empty sandbox worker, so the desktop
  comes up without waiting for a picture and a settings change does not freeze
  it. The icon rasteriser keeps the serve loop's own sandbox handle, untouched.
  The desktop keeps painting whatever it has until the new surface arrives; a
  picture prepared for a screen size or a choice the desktop has since left is
  discarded rather than stretched onto the wrong screen. See
  [the session](session.md).

A wallpaper that will not decode is not fatal: the desktop falls back to
the backdrop colour, reports why on `stderr`, and remembers the refusal, so
a bad file costs one attempt rather than one per frame.

A gallery that crawls on real storage is diagnosed by taking the file read
and the sandboxed render apart rather than by guessing: the two halves have
unrelated causes, and the decode is already a known quantity (a shipped
master decodes in about 15 ms at thumbnail scale and ~90 ms full-screen), so
a placement costing seconds is never the decoder.

The prepared picture is never cut to. Because it arrives whenever the worker
finishes — a second or so into the session at login, or mid-session when the
choice changes — it dissolves into whatever ground is on screen over the
theme's own `BackdropChange` span: over the backdrop colour at login, and over
the picture it replaces when the user picks another. See
[the session's backdrop crossfade](session.md#the-backdrop-dissolves-it-is-never-cut-to).

### Fits

| fit | what it does |
|---|---|
| `fill` | covers the screen, cropping the overflow, centred (the default) |
| `fit` | contains the whole image, letterboxed against the backdrop, centred |
| `stretch` | exactly the screen, ignoring the aspect ratio |
| `centre` | 1:1 in the middle, cropped if it is larger than the screen |
| `tile` | 1:1, repeated from the origin |

The geometry is one pure function in `lib/wallpaper`, shared by the desktop
and by every preview, so a preview can never disagree with the desktop about
what a fit will do.

## The icons

The pinboard lists the user's `Desktop` folder through exactly the same
machinery the file manager uses: the same directory seam, the same shared
sort, the same content-type classifier, the same grid, and the same
double-click activation ([desktop icons](./icons.md),
`plans/NEW-TASKBAR.md` T7/T16). It names and orders its icons as a
file-manager window does: less the endings the file manager's settings hide —
an application's `.app` by default — and by the names shown. Only the trusted
file picker shows exact names, since it is where an application is granted a
file. An item whose name begins with a dot has an icon only while the file
manager's settings show hidden items, off by default; one left out keeps the
place it was put in for when they are shown again.

An icon stands where the user put it, as on Windows or macOS: carried across
the desktop with the mouse, dropped or saved onto it — each file as far from
the drop as it was held — or laid out by *Tidy Up*. It lands exactly there,
over another icon if that is where it was put, and the one put down last is
the topmost, the one a click meets, whatever order the folder is sorted in.
Every other icon takes the next free cell of the grid, in the order the
folder is sorted, and keeps it until the screen, the order or the arrangement
changes. A place is the user's own: the session keeps it in its private
app-data scope, one key per icon placed and nothing for one never placed, so
only that user's desktop shows it and their next session shows it the same.
A place is measured from the edge the icons were arranged from when it was
put there, so icons kept against the trailing edge stay there on a wider or
narrower screen. A file renamed keeps its place, one deleted gives it up, and
a place the screen no longer holds whole — after a change of resolution or
scale — is shown on the free cell nearest it, never piled onto another at the
edge, and kept as it was. The arrow keys move to the nearest icon in their
direction, meeting icons stacked on one another in the order they are drawn.

Icons are selected as a file-manager window selects its items. A press on the
backdrop drags out a band, which selects every icon whose picture or name it
touches as it grows, beside what a press with `Ctrl` or `Shift` kept; `Escape`
takes back what it selected, and letting go keeps it. `Ctrl`-clicking an icon
toggles it and `Shift`-clicking adds it. Dragging one of several selected icons
carries them all, a click that does not drag collapses the selection to the
icon clicked, and a right-click inside the selection keeps it. *Open* and
`Enter` act on one icon, and with several selected they say so rather than
pick one. A step of the band repaints the icons whose mark changed and the
band's own edges, never the whole desktop.

A pointer over an icon changes nothing about it. An icon shows its selection,
the keyboard's focus ring, the lit folder an accepted drag would drop into, and
the lift of those being carried — never a hover look or a tooltip — and the
cursor stays the arrow, so moving the mouse across the desktop repaints nothing.

Three things are settings:

- **Arrangement** — the grid's cells fill a column downward and grow a new
  column across, starting either from the leading edge (the Windows/KDE
  arrangement, and the default) or from the trailing edge. The two are
  exact mirror images of one another in the shared grid engine, not two
  layouts.
- **Sort** — by name, kind, size, or date, drawn from the shared sort, so
  the desktop and the file manager agree on what each ordering means. It
  orders the icons no one placed.
- **Align to grid** — off by default; on, an icon put down settles onto the
  free cell nearest where it was let go.

A desktop icon is often a **shortcut** — a symbolic link the program
library's row menu asked the session to make (`plans/SYMLINKS.md` S5, see
[the session](./session.md#desktop-shortcuts)). It is classified and
activated as what it *names*: bundle-ness reads off the target's own leaf, a
folder or file is opened through the link, and one whose target has gone is
refused with its reason rather than launched blind. It is pictured where its
activation reaches what it names, so an application shortcut shows the
application's own icon.

An application or command bundle on the desktop shows its own icon, read from
its manifest and icon file and read again whenever either changes, however it
arrived — dropped from a window or copied with `cp` in a shell. An item a drop
is still writing is drawn by the picture its drag carried until its own is
read.

**Drives.** The desktop shows a drive for each attached volume of a kind the
file manager's *Show on the desktop* settings tick (`files.desktop.*`,
`tairix_browse::DesktopDrives`): removable drives alone at first, with the
view's root `/` counted as the system drive. A drive is drawn, never written
into the folder, labelled by its volume's title, else its catalog name, and
kept where it was put under its volume's identity, so it returns there. A
`/Storage` volume dragged onto the desktop leaves a shortcut instead
(`DropOperation::Link`): a link to `id::<identity>/` that opens the volume,
through `tairix_browse::volume::locate`, while it is attached, is drawn faded
while it is not, and says "“X” is not present" when opened then. A volume a
shortcut already shows is not drawn again as a drive. The session reads the
mount list off the loop on each mount-table notice — one read in flight, one
owed — and repaints only the icons whose presence moved. A drive's menu offers
*Eject* where its viewer may eject it and *Title* where they may retitle its
volume (`VolumeFacts::retitling_by`) — its field opening on the title, the
retitle carried out on the session's file worker as an eject is — and refuses
*Move to Trash* with the reason. The worker carrying an eject out first holds
the volume's lane off the icon readers (`lanes::hold`), so the decode under
way there ends and none begins, and holds the backdrop and slideshow
pictures read from the volume off the wallpaper preparers
(`WallpaperDesk::hold_within`) once the one being read ends; then it lets go
of each listing's watch on the volume and ends the follow of every bundle
shown there, so nothing the desktop holds refuses it as busy. With no worker
to hold, a picture or icon read on the session's own loop at that moment can
still refuse it as busy, and the eject says so.

**A name reads on whatever ground it stands on.** A resting icon wears no
plate, so its name sits straight on the backdrop. Over a picture, whose light
under any one name the desktop cannot know, the name is near-white over a dark
halo; over a flat backdrop it takes whichever of near-white or near-black
contrasts more with that colour, and needs no halo. The shell hands the paint
the ground it lays down (`Desktop::render`), so the ink changes when the
picture arrives rather than when the setting does. Only a resting name takes
that ink (`IconTile::with_label_ink`): a selected icon, or a folder lit for a
drop, keeps the theme's ink on its plate. The halo is clipped to the icon's
cell, so repainting a cell still covers everything it drew. Each name's halo is
blurred once and kept until the name shown, the scale or the theme changes, so
repainting the layer — every frame of a wallpaper crossfade — lays the halos
down again rather than blurring each anew.

**A folder no watch can follow.** The column follows its folder through a
[directory watch](../filesystem/watch.md). A volume that records no changes — a
network share, or the stand-in of a volume that has gone away — lists the
folder without one. The desktop says so once, on `stderr` and on the system log
(`PINBOARD_UNFOLLOWED`, 20036), until a watch joins again. It reads the folder
again after a save it made there itself (`Desktop::show_made`) and whenever the
mount table moves (`Desktop::relist_unfollowed`), which arms a watch once the
volume behind it records changes again, and once more as a drop onto it
ends, so a copy a dragging application makes there shows when its drop ends
(`plans/PINBOARD.md` P12).

## The backdrop menu

A right-click on the backdrop opens the desktop's own menu at the pointer. It
is a chain like every other menu on the system — the pinboard hands a row model
to the one [menu service](./menus.md) and keeps no shell of its own — and it
offers: `Open` (only when the click landed on an icon), `Eject`, `Title` and
`Move to Trash` (only over a drive, the last always refused with its reason),
`New Folder`, the four
sort orders, the two arrangements, `Tidy Up ▸ By Name / By Date / By Size`,
`Refresh`, `Open Desktop Folder`, and `Change Background…`, with the sort and
the arrangement in force shown as their group's chosen member and not offered
again. *Tidy Up* lays every icon out on the grid in the order chosen, from the
arrangement's corner, and keeps them there in one write.

Managing an entry — rename, copy, delete, properties — is deliberately
absent. Those verbs belong to [the file manager](./apps.md), which owns them
whole, and `Open Desktop Folder` is one row away; a half-implemented second
copy of them here would be duplication.

The menu never acts on its own authority. It names a command; the session
carries it out, and reports on `stderr` anything it could not do — a
refused folder creation, a Settings window that would not launch — leaving
the
desktop unchanged rather than failing silently or dying over a refusal. Where
each row's command is resolved, and how its one answer reaches the session,
is [the session's own page](./session.md#the-backdrop-menu).

## Choosing a picture

The desktop picture is a **section of Settings**, not an application beside
it: the menu's `Change Background…` opens
[the Settings application](./settings.md) at its Wallpaper pane, where the
fit, backdrop, arrangement, sort and *Align to grid* are five form rows above the shipped
pictures, filed under their categories and each drawn at the screen's own
16:9 shape with rounded corners. A Settings already running is handed the pane
and navigates to it; a fresh one is given the same pane and opens on it.

**Settings holds no authority over any of it, and gains none for the
pictures.** Listing the shipped store needs a filesystem capability and
decoding a picture needs a parser sandbox, and Settings requests neither —
an application that will later carry Networking, Users and Storage must not
also hold the reach to read arbitrary files. So the pictures are *served*: the
session lists the read-only store once at its own bring-up and answers a
catalog page on request, and renders each picture into a shared-memory
region Settings created and granted (`RenderPreview`, concluded
by `PreviewRendered`). There is one sandboxed decode path on the desktop
instead of two, and no picture is ever decoded in the address space of the
application that browses them.

A render names a **subject**, never a path — a catalog position, or a
screensaver's place in the catalogue the session lists, whose preview is the
OS's own under `/System/Graphics/Screensavers/` for Black and the dimmed
desktop and the saver bundle's `Resources/Preview.png` for any other — so the
request cannot make the session read a file the caller chose. It names the width and height to render at too,
within `WINDOW_PREVIEW_MAX_SIDE`, and the region must hold that many pixels. A
picture in effect that the catalog does not hold — one set before it was
removed from the store, or from outside it — is still offered and still
selectable, beside the others of its category; it simply has no position to
be rendered at, so its tile draws its built-in glyph and its name.

The desktop renders previews on its preparers — one per online CPU, each with
its own sandbox — but **memory**, not the preparer count, bounds how many run
at once. Renders share a budget sized from the machine's memory and its
pressure band: the share `lib/reclaim` grants work begun speculatively
(`speculative_budget`), so renders begun on a machine with room stop short of
pushing it into moderate pressure. A render starts holding only what it is
known to need before its picture has been read: the largest source and the run
it is uploaded in (`WALLPAPER_PREPARATION_BYTES`). The worker then reads the
picture's header and reports what decoding it at the size asked will hold
(`plan_wallpaper`, priced by `tairix_image::decode_peak_bytes`). The render
acquires that figure if it fits beside the renders under way. If it does not,
it goes back to the head of the queue with its cost known, and starts again
only once that cost fits — or once nothing else runs, since one render always
makes progress whatever it costs and runs with nothing beside it. A screenful
of small thumbnails therefore decodes side by side while a large picture
decodes alone, on a small machine or a large one. At critical pressure no
render starts. The desktop's own backdrop and the screensaver's slides hold
the figure their plans report like any render, without waiting, and the
backdrop is always prepared first: the picture the user is looking at never
waits behind a thumbnail.

A conclusion says how the render ended (`PreviewOutcome`). A render that runs
out of memory beside others is queued again to run alone; one that runs out of
memory alone, or whose plan the desk withholds memory from — at critical
pressure, or as the desk stops — is answered `Unavailable` at once, and
Settings asks for it again once memory may have been freed
— another picture fitting, or the pressure band moving. A picture the session
cannot read or decode is answered `Refused` and keeps its placeholder.

A window may have no more renders pending than there are preparers, which
bounds how much decoding a browsing application can set going. A request past
that is answered `LimitExceeded`, and the application asks again when a render
concludes. Settings carries each request to the session on a worker of its own,
one round trip at a time, so its window never waits on the session's serve
loop. A window that closes takes what it still has waiting with it; a render
already under way for it is refused its memory, never queued again, and
finishes into nothing. Every picture is requested and
never awaited — a paint draws what has come back and a placeholder for what has
not — so the pane is usable from its first frame and fills in as the answers
land, those on screen first.

**An apply does not block the window.** The session answers only once its own
publisher has written the store, so the choice is rendered into the pinboard
half of the settings document (in memory, and refusable on the spot) and the
round trip is handed to a worker. The rows and the pictures stay live
throughout, and the answer is what becomes durable: a refusal puts the
selection back rather than leaving a picture on screen the next login would
not restore.

## Headless

The pinboard is part of `userland/gui/*` and is therefore optional in
exactly the way the rest of the desktop is (`AGENTS.md` §17.3). A headless
image omits it, and nothing outside `userland/gui/*` depends on it. The
`lib/*` crates it rests on — the settings engine, the decoder, the
resampler, the sandbox — carry no GUI dependency and are useful without it.
