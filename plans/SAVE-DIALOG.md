# SAVE-DIALOG.md — Saving a document: the Save panel and drag to save

Binding under `AGENTS.md`. The trusted picker's save (`plans/APPWIN.md` AW5,
`plans/CAPABILITY_USE.md` CU6) is the one way an application holding no
filesystem authority gets a file to write. This plan is how that save looks
and works: a compact panel that names the document, its place and its format
in one glance, a document icon the user drags straight onto a folder to save
there, and the full folder browser one press away.

## Ledger

| id | What it is | Status |
|---|---|---|
| SD1 | The Save panel: the document's icon, its name with the stem selected, *Where* (the folder it goes in) and *Format* (the format it is written in), Cancel and Save, *Show folders* expanding the browser with *New Folder*, and every control on the keyboard | done |
| SD2 | *Where* and *Format* as session menus (`ChainOwner::Picker`): the user's own places and the folder last chosen; each format offered by the registry's own name for its type | done |
| SD3 | Drag to save: the panel's icon carried by the seat's drag carrier as a save, shown as the document's large tile held where it was pressed, dropped on the desktop or a folder in a file manager window to save there | done |
| SD4 | The save offer: `SaveOffered`, `SaveOfferAnswer`, `SaveOfferEnded` and `SaveInto`, the drop target naming the folder by a location grant it minted itself | done |
| SD5 | The file manager as a save target: the folder under the pointer lit while the offer stands, its location granted off the loop on the drop | done |
| SD6 | Formats, not endings: the requester offers each format it writes — a RISC OS file type and the extensions that name it — and the one the document is in, which the panel opens on; the file is typed by its volume where that records file types, by `,xxx` where not, so the requester tells the format chosen from the file it is handed | done |

## 1. The panel (SD1)

- **One window, two extents.** The panel opens compact: the icon on the left,
  then *Name*, *Where* and *Format* (stated when the requester writes one
  format, a choice when it writes more, absent when it writes any name), a
  line for a message, and *Show folders*, *Cancel* and
  *Save* along the foot. *Show folders* swaps the window's surface for the
  expanded one (`Compositor::set_surface`): the same controls over the shared
  listing (`lib/browse`) and *New Folder*. Collapsing keeps the folder shown.
- **New Folder** names the folder in the name field — the first `New Folder`
  name the listing does not hold, selected; the label reads *Folder* and *Save*
  reads *Create* — so naming a folder needs no window of its own. A name the
  listing holds is refused before anything is made; the folder is made on the
  file worker (`PickStep::NewFolder`, answered by `folder_made`), and the save
  then goes into it with the document's name back in the field. `Escape` or
  *Cancel* while naming goes back to the document's name, not out of the pick.
- **A dialog of the requester's window.** It opens over the window that asked
  (`DesktopShell::open_dialog_window`), not at a fixed point, so it reads as
  that document's question. The open and folder picks are windows of their own
  at a fixed geometry, the first at `PICKER_ORIGIN` and each further one up at
  once in the lowest free cascade slot (`picker_origin`), which the QEMU picker
  vertical reconstructs.
- **Modal to its requester alone.** Each pick holds the window that asked, as
  an owner-modal dialog does (`SessionWindows::hold_for_pick`, one hold per
  pick): a press on that window brings its picker forward, and its menus,
  raises and drags are refused. A picker holds the seat only while it has the
  keyboard (`seat_held`), so every other application stays usable and may ask
  for a pick of its own, and the desktop's own menus and program library open
  over it even then (`seat_modal`), handing the keyboard back as they close. A
  save takes the keyboard as it opens only when its application holds the
  front, and otherwise opens beneath the window the user is in; when the pick
  ends the keyboard goes back to the requester only if the picker had it.
  While any pick is on screen it is still a trusted surface (`trusted_up`), so
  the desktop layer surface stays hidden.
- **The name.** Offered as the requester suggests it, the stem selected so
  typing replaces the name and keeps the ending; an ending the registry names
  another type by is left off, so a picture saved in another format is not
  offered under its old type's name (`offered`). On Save the name is held to
  the chosen format (`SaveBand::held`): one ending as the format — one of its
  extensions, its own `,xxx`, or one the registry names its type by — is kept
  as typed; one of another type is refused, saying how to name it; one with no
  ending takes the format's first extension, or, for a format named by its
  file type alone, is left for the folder to type.
- **A name taken where no listing showed it.** An exclusive create refused
  because the name exists is told apart on the file worker: a file is asked
  about as any other, while a folder or a link holding the name is refused
  (`IsADirectory`, `LinkLoop`), since no save replaces either.
- **The format (SD6).** `PickPurpose::Save` carries `SaveFormats`: at most
  eight `SaveFormat`s — a RISC OS file type, or none, and at most four
  extensions — and the one the document is in now, which the panel opens on
  whatever the name ends in. No two may share a file type or an extension, and
  the decoder refuses a frame that misstates any of it. The panel names a
  format by the registry's name for its type and draws its type's picture, never
  a label the requester chose; typing an ending of another format offered
  chooses it, and choosing one renames the stem with its extension. A requester
  builds a format from the registry (`tairix_browse::media::save_format`): its
  file type, and its extensions but for a type RISC OS is home to and other
  systems name by an extension (`FILETYPE_NAMED`: a sprite file is named
  `Sprites`, not `Sprites.spr`).
- **The file's type.** Whether a volume records file types is known only on
  the file worker (`tairix_browse::vfs::keeps_filetypes`), so the worker makes
  the file (`open_save`): it opens the folder, finishes a bare name with the
  type's `,xxx` where the volume records none (`Typing::leaf`), creates the file
  relative to the folder it asked so the folder cannot change between the
  asking and the making, and records the file type where the volume records one,
  before any requester holds it — a type it cannot record refuses the save and
  takes back a file it made. A bare name is created exclusively without a
  listing check, its name being the folder's to finish, and a name found taken
  is asked about by the name the file would have had. The requester tells the
  chosen format from the file it is handed, typed so before it holds it, and
  no two formats offered share a type or an extension: Paint writes the format
  the file's name and recorded type say (`save::format_for`).
- **The folder.** *Where* names the browser's folder. The panel starts where
  the picker starts (the user's `Home`) and the browser behind it is the one
  the expanded panel shows, so a folder chosen either way is the same one.
- **Save.** Exactly the save band's conclusion: a name no entry shows is
  created exclusively, one that names a file is replaced only after the alert
  asks, one that names a folder goes into it, and the file is delegated
  write-only (`PickAccess::save_flags`). The open runs on the file worker, the
  panel saying *Saving…* until it is answered.
- **Keys.** `Tab` and `Shift+Tab` walk the name, *Where*, *Format*, *Show
  folders*, the listing and *New Folder* while the browser shows, *Cancel* and
  *Save*, each drawing its focus ring; `Enter` or `Space` answers the control
  the keyboard is on and `Down` opens a chooser. On the name `Enter` saves; on
  the listing `Up`, `Down`, `Enter` and `Backspace` move, enter a folder or
  offer a file's name, and climb. `Escape` cancels, and `Ctrl+Shift+N` names a
  new folder while the browser shows.

## 2. *Where* and *Format* (SD2)

- Both open a menu chain the session owns (`ChainOwner::Picker`), anchored on
  the control, so they are drawn, navigated and dismissed exactly as every
  other menu is (`plans/NEW-MENUS.md`).
- *Where* lists the folder shown, then the user's own places — `Home`, its
  `Documents`, `Pictures`, `Music` and `Videos`, and `Desktop` — and the folder
  the last save went to, each by its name and its folder picture. The picker
  is not bounded to them: the browser reaches every folder it lists, so the
  list offers nothing a navigation could not.
- *Format* lists each format the requester writes, by its type's name, the
  chosen one marked.

## 3. Drag to save (SD3)

- **The icon is the document.** A press on it carried past the drag slop
  begins a *save* drag on the seat's one drag carrier (`drag.rs`): the
  document's large tile — its picture above its name, as a listing draws it —
  held where the press held the panel's picture (`PictureGrab`), the pointer
  showing whether a drop there saves.
- **Where a save drag lands.** On the desktop: the folder the pinboard shows
  there, a folder icon's or the `Desktop` folder itself, resolved by the
  session. On a window of an application that answers the offer (SD4): the
  folder it names. Anywhere else, or on the panel or its requester: nothing.
- **Dropped.** The panel saves into that folder under the name it shows, as
  *Save* does into the folder shown — the replace question included — and the
  panel stays up, saying *Saving…*, until the open is answered.

## 4. The save offer (SD4)

- `WindowEvent::SaveOffered { window_id, serial, x, y }`: a document being
  saved is held over the window at a window-local point. Numbered; the owner
  answers with `WindowRequest::SaveOfferAnswer { window_id, serial, accepts }`,
  shown on the pointer while it still describes where the pointer is.
- `WindowEvent::SaveOfferEnded { window_id, serial, dropped }`: the offer left
  the window (`None`), or was dropped on it at the window-local point
  `dropped` names — carried, so the answer depends on no offer the owner
  missed. On a drop the owner answers once with
  `WindowRequest::SaveInto { window_id, serial, location }`, `location` a
  `fd_grant_location` handle of the folder **it opened itself**, minted to the
  session; `location` zero declines.
- **Why a location grant.** The session creates the file under its own
  authority, so the folder must be one the drop target could reach itself: a
  target holding no filesystem capability cannot name a folder at all, and no
  application gains reach through someone else's save. The session redeems the
  location bound to the window's attested owner, so a handle another process
  minted is refused.
- An answer is honoured only for an offer made since the pointer reached the
  place it is over, so one about where the pointer was is never taken for
  where it is. A drop waits for its `SaveInto` with the panel saying so and still
  answering *Cancel*, so nothing is timed; the window closing ends the wait,
  saying so. A location that arrives for an offer no longer standing is
  redeemed and let go, so no grant is left pending in the kernel.
- **Over the desktop the session decides itself**: the folder under the pointer
  — a folder icon's, or the `Desktop` folder — takes the save at once and is
  lit, and nothing is offered to any application.
- **A drag outlives no part of its save.** The window the save is for closing
  ends the drag, and the window last offered it hears the offer end; a window
  offered it closing is forgotten and the drag carries on. A save never offers
  itself to the window it is for, a window an alert holds, or a window's
  furniture.

## 5. The file manager as a target (SD5)

- A `SaveOffered` over a folder tile, or over a window whose listing is a
  folder, is accepted and lights that folder as a file drag does
  (`drop_folder_at`, the one drop policy); over the network view, the root
  view or the storage catalog (`tairix_browse::vfs::stores_files`), or anything
  not a folder it is declined.
- On the drop the folder at the drop point is opened on the reader serving its
  volume, never on the loop (`ReadKind::Save`, after a document's open), its
  location minted to the session, and `SaveInto` sent; a folder that will not
  open declines.
- **A slow target keeps up.** The session's hold-back folds a run of offers to
  the newest, an offer's end withdraws the offers still held, and an end that
  only left supersedes an earlier one, so a window that stopped draining its
  mailbox owes what it still needs to know, not every motion.
