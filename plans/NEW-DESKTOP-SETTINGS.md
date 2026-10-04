# NEW-DESKTOP-SETTINGS.md — the desktop Settings application

Binding under `AGENTS.md`. This is the staged build plan for **Settings**
(`os.tairix.settings`), the windowed application that configures the desktop
and the machine, opened from the *Settings…* row of the Switchboard capsule's
system quick-actions menu.

Read first, in order: `AGENTS.md` (all of it, §2, §4, §5, §10, §16.5, §17.4),
`plans/GUI-CONTROLS-DESIGN.md` (the Reactive Alloy vocabulary every surface
here composes — no second control implementation), `plans/NEW-SWITCHBOARD.md`
(the sibling system surface, and the boundary in §0 below),
`plans/NEW-TASKBAR.md` T13 (the system quick-actions menu this is launched
from), `plans/APPWIN.md` (the window channel, the app-owned popup surfaces),
`plans/APPS.md` (§2 bundle layout, §2.1 the `Help/` locale tree, §14 the
mandatory app icon), `plans/APPDATA.md` (the per-app store the session's
settings live in), `plans/PINBOARD.md` §6 (the apply-rendezvous pattern every
user-scope write here reuses), `plans/ICONS.md` (§0, the mandatory built-in
glyph tier), and `plans/CAPABILITY_USE.md` (capability sizing). Every rule in
all of them applies here without exception.

**Note:** `abi-v1` is not frozen. A `lib/abi` change is allowed and requires
regenerating the C header (`cargo xtask c-header --write`), which the drift
guard enforces.

## Ledger

Every stage this plan calls for, what it waits on, and where it is specified.
A stage is `done` only when its host tests, its rustdoc and `docs/` page, and a
green whole-workspace gate landed with it. Nothing here is optional: a stage
dropped is a category the surface then has to lie about.

| # | Stage | Depends on | Spec | Status |
|---|---|---|---|---|
| **DS1** | `lib/controls::form` — `FieldRow`/`FieldGroup`/`FieldControl`/`FieldLayout` over the row chrome hoisted into the shared `paint` core, the measured-width accessors the slot model needs, the caption-line badge, and a `widgets.app` gallery tab | — | DS1, §4 | done |
| **DS2** | The `userland/apps/settings` crate and its shell: the closed `Category`/`Pane` registry, the vertical `Tabs` sidebar, the search index, the breadcrumb band, frame shedding, the absence-pane renderer, and the taskbar's *Settings…* row | DS1 | DS2 | done |
| **DS3** | Appearance and Accessibility over the session's user-scope appearance registry, and the apply rendezvous every other user-scope write reuses | DS2 | DS3 | done |
| **DS3b** | The cursor pair: a cursor-set store under `/System/Graphics/Cursors/<set>/` so `cursor.set` has a choice space at all, and a `cursor.size` factor in the session's cursor controller | DS3 | DS3b | done |
| **DS4** | Wallpaper — the gallery absorbed into the pane over two served requests, `wallpaper.app` deleted, and *Change Background…* opening Settings at that pane | DS3 | DS4 | done |
| **DS5a** | The shared volume view model: the mount record → capacity/health derivation in `lib/procinfo`, the `VolumeHealth` banding in `lib/abi`, the one band→role binding in `lib/theme`, and every private copy collapsed onto them | — | DS5a | done |
| **DS5** | Storage — one card per mount with its capacity track and health pill, over the DS5a model | DS2, DS5a | DS5 | done |
| **DS6** | The elevated-apply seam: `ElevateRequest::Run` gains a bounded argv, and General (About, Login & startup, Caching, Date & Time) is its first consumer | DS2 | DS6 | done |
| **DS7** | Networking read — the stack-wide `net.*` options staged and applied live, the ungated resolver set stated, and the gated per-interface readings left where they may be taken | DS2, DS6 | DS7 | done |
| **DS8a** | The elevated-read seam: an `ElevateRequest` whose reply carries the bounded output of the run, so an authenticated account can *show* a store no unprivileged caller may read — with `configure`'s per-interface **read** registry and the Ethernet pane that states it | DS6 | DS8a | done |
| **DS8** | The network store's writer — `lib/netconfig`'s draft/commit mutation API, `configure`'s write side and its *unset* spelling, the live apply over the stack's admin surface, and the device manager's runtime re-read | DS6, DS7, DS8a | DS8 | done |
| **DS8b** | Ethernet and DNS stage and apply through that writer — the addressing reading becomes a settable per-interface form, and Apply is the one elevated `configure` run | DS8 | DS8b | done |
| **DS9a** | The three reads' plumbing and the command family they and the write side are driven through: the ungated `GROUP_DIRECTORY` and `SELF_ACCOUNT` queries end to end, the shared `lib/useradmin` client, `users --list`, and the `usermod`/`userdel`/`passwd`/`groupdel` bundles | DS6, DS8a | DS9 | done |
| **DS9** | Users & Groups — the pane itself: the three reads composed, the per-account staged edits, and the one elevated run that applies them | DS9a | DS9 | done |
| **DS10** | Notifications — a per-source allow/deny and minimum severity enforced at the session's one `NotifyRequest` intake | DS3 | DS10 | done |
| **DS11** | Keyboard and Mouse — the session's pointer and key-repeat policy, and the one double-click interval it publishes for every app | DS3 | DS11 | done |
| **DS12** | Lock Screen and Screensaver — the session's single idle deadline and the one timer armed only while a policy has one pending | DS3 | DS12 | done |
| **DS13** | The `settings_qemu_aarch64` vertical and the docs pages the surface owes | DS2–DS9 | DS13 | done |
| **DS14** | Retire the second form idiom — `datetime.app`'s six-field row and `lib/browse`'s `PermGrid`, with the private layout arithmetic each carries deleted | DS1 | DS14, §6 | done |
| **DS15** | The sidebar's category badges: colour built-in pictures at the theme's sidebar icon size, on rows tall enough to seat them, retained in the window's own icon cache and trimmed on the memory-pressure wake | DS2 | §4, `plans/ICONS.md` I13 | done |
| **DS16** | The window is cut from the icon bar's glass, frosted deeper: the bare ground and the command band at `chrome_alpha` over `window_backdrop_blur`, everything on them solid, and what the shell opens over its content drawn opaque (`Grounds`); the vertical's absence check reads the ground the production compositor draws | DS2, DS13 | §1.1 | done |
| **DS17** | The sidebar as one grouped plate: the search field and the strip on a rounded plate, the categories in runs (`Group`) set apart by half-row breaks, a badge on every row including each disclosed pane, lists that open independently (`lib/controls::DisclosureSet`, the program library's folders on it too) with the tree keys, and the Theme category's stated absence | DS2, DS15 | DS17 | done |
| **DS18** | Screensaver scenes and energy saving: the clock, minimal clock, starfield and Game of Life screensavers, the pointer hidden beneath every one, and the display switched off a set wait after the screensaver starts | DS12 | DS18 | done |
| **DS19** | Pictures as settables: the wallpaper and the screensaver chosen by their pictures (`lib/controls::PictureChoice`), categorised and at 16:9 with rounded corners; a shipped preview per screensaver; the chosen screensaver's own options; and *Test*, the session's preview | DS4, DS18 | DS19 | done |
| **DS20** | Finding the pointer, and input set in words: shake to find (on by default), pointer trails, finding it with Ctrl, and a pointer shadow on Accessibility; the Mouse and Keyboard spans as sliders from *Slow* to *Fast* over the redesigned knob | DS3b, DS11 | DS20 | done |
| **DS21** | The ray-traced screensaver (`screensaver.kind` = `raytrace`): scenes from `lib/raytrace` prepared and revealed coarse to fine on a tracing thread of its own with a progress readout, held, faded and replaced, on one core or every core as `screensaver.raytrace.cpu` sets, each finished picture kept when `screensaver.raytrace.save` asks | DS18, DS19 | DS21 | done |
| **DS22** | The retro games screensaver (`screensaver.kind` = `retro_games`): a flight over a glowing grid towards a banded sun between two wireframe ranges, its reflection rippling on the floor, at a speed on the shared `Pace` ladder, where wireframe craft now and then play out retro arcade games — a starfighter, a flying saucer, a tank battle, and riders walling each other in | DS18, DS19 | DS22 | done |
| **DS23** | The System Monitor screensaver (`screensaver.kind` = `system_monitor`): the machine's own readings set out to be read from across a room — processors, memory, tasks, storage and network under a verdict naming what needs attention — from the Switchboard's machine report, with whether the busiest tasks are named as its one option | DS18, DS19, `plans/NEW-SWITCHBOARD.md` M3 | DS23 | done |
| **DS24** | The ray tracer's scene detail: `screensaver.raytrace.detail`, `simple` (the default) or `maximum`, a *Detail* dropdown in its group on the pane reading *Simple* or *Maximum realism*, carried to the tracing host at launch and falling back to Simple where the memory band says Maximum's peak is not free | DS21, `plans/RAYTRACE.md` RT39 | DS24 | done |
| **DS25** | Trackpad — tap to click, natural scrolling and the touchpad speed (`touchpad.*`), handed to the seat's gesture recogniser; the Touchscreen pane states what it would need to set anything | DS11 | §2, §3; `plans/POINTING.md` PO6 | done |

**DS9a, the plumbing the pane composes.** DS9's read half needs three
answers of different authority, and its write half needs tools an
elevated run can actually drive; neither existed, and both are
independent of how the pane draws them. They landed first, as DS9a:

- `SysinfoQueryId::GROUP_DIRECTORY` (ungated, paged, fixed-width) and
  `SysinfoQueryId::SELF_ACCOUNT` (ungated, self-scoped), each with its
  `lib/abi` frame in the fuzz sweep, its kernel introspect domain, its
  `sysinfod` seam, and its `lib/procinfo` client. The group directory
  needed a live registry the kernel could serve names from, so the
  unlock now publishes `groups-v1` into a `LateGroupsDb` the audited
  admin engine swaps on every commit — a created group is visible to a
  display as soon as it is durable, not at the next boot.
- `lib/useradmin`, the one `users_admin` client every account tool now
  shares, including the `:`-delimited relayed-listing form whose
  renderer and parser are one definition.
- `users --list`, the non-interactive read the pane's capture drives:
  the interactive session cannot serve one, because `ElevateRequest::Capture`
  closes the child's standard input.
- `usermod`, `userdel`, `passwd` and `groupdel` as `plans/APPS.md`
  command bundles, completing the shadow-utils family beside `useradd`
  and `groupadd`.

**DS9, the pane.** Three plates from those three reads: the caller's own
record, the accounts the authenticated listing answered (or the public
roster and a footnote naming what authenticating adds), and the group
directory. The listing is the band's *capture* — one `users --list` run,
read back through `lib/useradmin`'s one listing form — and is dropped
when the reader leaves the pane; a capture that lands for a pane the
window has since left is dropped rather than installed, because the
desktop can send the window elsewhere while a run is in flight. Apply is
**one** elevated run, and a change spanning two accounts or a password
beside other fields is refused with its reason before a password is
typed. A run that exits cleanly moves the listing on to hold what was
applied rather than dropping it, so a second change costs a second
authentication and not a second reading. A service identity's home,
shell, login and password are readings, because the database refuses a
record shaped otherwise. The plaintext of a new password lives only in
its masked entry — carried across a rebuild by *moving* the control, so
no second buffer ever holds it — and is read by borrow at the moment it
is hashed.

Two decisions the pane inherits, recorded here so they are not
re-derived:

- **Every Users write elevates a *named* account and the kernel decides.**
  `users_admin` is gated whole on `CAP_USER_ADMIN`, so a principal
  editing *its own* record still needs that grant; a genuinely
  unprivileged self-service password change would be a new authority
  path, not a widened gate, and is out of scope here. The pane offers
  the action, the kernel refuses it where it must, and the pane states
  the refusal.
- **`passwd` takes a ready record.** The pane hashes with the shared
  `lib/users` builder and passes `--record`, so no plaintext leaves the
  Settings process and none rides an argv. The password row is the shared
  masked entry (`lib/controls`' `SecretField`), never a visible one, and
  says `[input active...]` as the console's prompt does. The
  salt is drawn by the caller from the kernel CSPRNG and held one deep:
  an apply spends it and the caller draws another, and an apply with none
  held is refused rather than salted predictably.

**The honest shape of the deliverable.** Six of the categories the desktop
should offer have no subsystem beneath them today: there is no Bluetooth stack,
no print/scan stack, no touchpad or touch input driver, no 802.11 driver, and
no file/screen sharing server anywhere in the tree. Sound has its stack but no
device control — nothing sets a device's volume or the default device — so it
is a seventh category with nothing to set. Theme is an eighth: the appearance
and the picture it would gather are each set on their own panes, but nothing
names them together and there is no accent palette to choose. Settings cannot
invent any of them,
and it must not draw a volume slider that changes nothing — that is the
fabricated-reading defect the whole desktop is built to avoid. So
those categories are **present, reachable, and honest**: each states what is
missing and what would have to land, exactly as the Switchboard's Services and
Power pages already do. §3 is the table of them; each row names the plan that
would fill it. Every other category is backed by a real reading and a real
write path on landing.

---

## 0. Scope and decisions (binding for this plan)

- **Switchboard observes; Settings changes.** The two surfaces are not
  siblings with overlapping content and must never become one. The
  Switchboard reports what the machine *is doing* — tasks, pressure, faults,
  live per-core load, mounted volumes' health — and its commands act on
  running things (raise, pause, force, restart a task). Settings changes what
  the machine and the desktop *are configured to be*, and holds no command
  that acts on a running task. Where both want the same fact, the fact has
  one reader: the shared System Information API query. Where both would want
  the same *command*, only one has it — the machine's power transitions stay
  in the system quick-actions menu, behind its existing confirmation, and
  Settings' Power pane does not offer a second route to them
  (`AGENTS.md` §2.2, and macOS parity: shutting down is not a settings pane
  there either).

  Concretely, the one reader is `lib/procinfo`: it already owns the paged
  walks over `MOUNT_LIST`, `USER_DIRECTORY`, the process list, the CPU-time
  stats and the pressure fetch, and both the Switchboard and the CLI tools
  read through it. Settings adds no sampler of its own, and no derivation
  either: a mount record becomes a capacity reading in `lib/procinfo`'s
  `volume` module (DS5a), which every surface reads, rather than each
  keeping a private copy that will drift.

- **Settings holds no domain authority. It never holds any.** Its manifest is
  `CAP_CONSOLE_WRITE` + `CAP_SHM`, and nothing else — the same class as the
  widget gallery. It does **not** hold `CAP_TIME_SET`, `CAP_NET_ADMIN`,
  `CAP_USER_ADMIN`, `CAP_SYSTEM_POWER`, `CAP_STORAGE_ADMIN`, `CAP_DISPLAY`, or
  `CAP_FS_MOUNT`. It does not hold `CAP_USERS_READ` either, which is the
  sizing decision worth naming: that capability gates `users_db_read`, whose
  answer is the whole `users-v1` credential database **including every
  password record**. A settings browser that wanted to print a user's full
  name would be holding every hash on the machine. So the account roster comes
  from the ungated, credential-free `USER_DIRECTORY` query instead (DS9).
  This survives absorbing the wallpaper gallery (DS4), which is the one place
  it came under real pressure: browsing pictures needs the store listed and
  each one decoded, and the chooser held `CAP_FS_ACCESS`, `CAP_PROC_SPAWN` and
  `CAP_LOG_EMIT` to do it. Settings does not inherit them. The session already
  owns a sandboxed image renderer, so it serves the catalog and the previews
  and Settings asks — which keeps the manifest at two capabilities *and*
  leaves one sandboxed decode path on the desktop instead of two.
  An application that could change everything is precisely the ambient-
  authority god-app `AGENTS.md` §4 and §5.2 forbid, and a settings *browser*
  does not need to be one: every change is either a request to the process that
  already owns that domain, or a re-authenticated run of the tool that already
  writes that store. §2 is the authority map, pane by pane. A pane whose write
  path is refused states the refusal and changes nothing (`AGENTS.md` §2.24) —
  it never reports a success it did not get.

- **Three write paths, and no fourth.** Every settable in this plan reaches
  one of exactly three owners:

  1. **User scope → the desktop session.** Appearance, contrast, density,
     reduced motion, UI scale, cursor set, wallpaper and pinboard keys,
     notification policy, pointer and idle policy. Settings renders the
     document and posts it to the session, which validates it, applies it, and
     persists it to its own published app-data scope. This is the
     `plans/PINBOARD.md` §6 rendezvous exactly as the backdrop menu already
     uses it: the session is the only writer, an application publishes
     only its own scope, and the desktop adopts a change **only after the
     write succeeded**, so memory and disk cannot diverge. That write happens
     on the session's settings worker, never on its serve loop, and the same
     rule binds this application's own panes: a control's value is never wired
     to a write, a continuous control acts durably only where its interaction
     settles, and no pane ever blocks its window on a store (`AGENTS.md` §28).
     A slider that posted its document per pointer sample would freeze both
     this window and the desktop's.
  2. **Machine scope → the tool that already writes that store, run as an
     authenticated account.** `system.conf` and `network.conf` have exactly
     one writer engine each (`lib/sysconfig`, `lib/netconfig`) and one command
     app over them (`configure`). Settings does not grow a second writer: it
     asks the console's elevation broker to re-authenticate an account that
     may and run that same program (DS6). The CLI and the GUI are then
     literally the same writer and cannot diverge.
  3. **Kernel scope → the syscall's own tool, run as an authenticated
     account.** Users, groups, grants and passwords go through the
     `users_admin` syscall, whose gate is `CAP_USER_ADMIN`; the clock goes
     through `CAP_TIME_SET`. Both are reached by elevating the tool that owns
     them — the user-admin command family and `datetime.app` — never by
     Settings acquiring the capability.

- **A pane is a form, and the form family is shared.** A settings pane is a
  scrollable column of captioned groups of label/description/control rows.
  That shape is not this app's to invent privately: the file manager
  hand-rolled a permissions grid, the wallpaper surface a column of four
  drop-downs, and `datetime.app` a row of six fields. `lib/controls::form` is
  the one family (DS1): `datetime.app` and the file manager's Permissions tab
  are drawn with it (DS14), and the wallpaper copy went with the application
  that carried it (DS4), because two form idioms in one desktop is the
  duplication `AGENTS.md` §2.2 forbids. The family composes the existing row
  chrome and control families; it re-implements no plate, press, focus,
  disabled or Authority-Mark rendering.

- **The pane registry is one closed table, and it is data.** `Category` and
  `Pane` are closed enums; one ordered `CATEGORIES` table is the single
  definition of the sidebar, the search index, the location trail, the
  keyboard cursor, and the pane dispatch. A pane cannot exist without a row,
  or a row without a pane. Adding a category is adding a row and a renderer,
  never touching the shell.

- **Absence is stated, never mimed.** Three distinct statements, and the
  surface never blurs them, because they are different facts to a reader:
  *no interface exists in TAIRiX yet* (the pane says so and names what would
  have to land); *the interface exists but this machine has no such hardware*
  (an empty list with the absence named); *the interface and the hardware
  exist but this caller may not change it* (the control keeps its value and
  wears the Authority Mark, with the reason stated). A control that would
  change nothing is never drawn as though it would.

- **Every reading is a measurement.** Settings reads the live machine through
  the System Information API and the config-store engines, never through a
  pseudo-file and never through a remembered value it hopes is still true. A
  reading it could not take renders unmeasured, never a fabricated zero or a
  default presented as the truth. It re-reads on window focus and after every
  applied change, so what it shows is what is.

- **One window, one instance, and no icon-bar slot.** Settings is part of the
  desktop rather than an application the user manages: its signed manifest
  presents no icon-bar slot, so closing the window ends the program — there is
  no slot left holding a handle on a windowless process. It is a singleton,
  which is the manifest's own default and is what makes *Change Background…*
  able to reach the Settings a user already has open (DS4) rather than
  starting a second view of one machine's configuration, each able to
  overwrite the other's applies.

- **One window, resizable, server-decorated.** The compositor draws the title
  bar, frame, and window commands; Settings' content is the whole client
  (`plans/COMPOSITOR-WORK.md`). It re-maps its zero-copy frame region on
  `WindowEvent::Resized` and lays the shell out to the new viewport. No modal
  maze: a pane's every control is on the pane, and the only overlays are the
  shared `Menu`/`ComboBox` popups and one `Dialog` for a destructive
  confirmation.

- **Fail closed, park never poll.** The event loop parks on the wait set; a
  pane that is not on screen samples nothing; a refused read leaves the pane
  exactly as it was and states why; a malformed or refused apply changes
  nothing anywhere. No store read or write and no request another service
  answers runs on the loop: each is a worker's, its answer adopted on the
  wake it nudges — an apply and the read of what the store then holds, the
  readings, and the session's lock, screensaver preview and notified-sources
  query, on one desk holding one of each, a newer preview held behind the one
  outstanding (`asks::DesktopAsks`). What the first frame needs is read
  before the window opens. The window channel's own requests — the present,
  the title, the open-target pull — stay on the loop, as every
  application's do (`plans/OPEN-DEFECTS.md` D653).

- **Not in this plan:** the audio, Bluetooth, print, touch, wireless, and
  sharing subsystems themselves (§3 names each one's prerequisite); the
  compositor's window furniture; display-mode setting (there is no mode-set
  request in `display_ipc`, §3); civil time zones (`plans/TIMEZONES.md`);
  and the service manager (`plans/NEW-SERVICEMANAGER.md`). This plan consumes
  those surfaces where they exist and states their absence where they do not.

---

## 1. The surface

### 1.1 Chrome and navigation

The window is drawn on `WINDOW_GROUND` (`SurfaceGround::Frosted`): its bare
ground and the command band let the blurred desktop through at the icon bar's
weight, and everything laid on them — the sidebar's rows, every group, field,
button and picture — is solid. A
choice list, the category menu and the credential question stand over the
window's content rather than on the glass, so `Grounds` draws them on the
opaque theme. The window manager's frame and title bar stay opaque.

```
 ┌──────────────────────────────────────────────────────────────────┐
 │ ╭───────────────────╮  Settings › Networking › Ethernet          │  band
 │ │ [search field]    │  ┌──────────────────────────────────────┐  │
 │ │ ⚙ General       ▸ │  │ CONNECTION                           │  │
 │ │                   │  │  Status              Connected       │  │
 │ │ ◑ Appearance      │  │  Configure IPv4      [DHCP      ▾]   │  │
 │ │ ▤ Wallpaper       │  │  IP address          10.0.2.15      │  │
 │ │ ◍ Theme           │  └──────────────────────────────────────┘  │
 │ │                   │  ┌──────────────────────────────────────┐  │
 │ │ ▭ Displays        │  │ DNS                                  │  │
 │ │ …                 │  │  Servers             10.0.2.3    [+] │  │
 │ │ ⇅ Networking    ▾ │  └──────────────────────────────────────┘  │
 │ │    ⧉ Ethernet     │                                            │
 │ │    ◠ Wi-Fi        │                                            │
 │ │ …                 │                              [ Apply ]     │
 │ ╰───────────────────╯                                            │
 └──────────────────────────────────────────────────────────────────┘
```

- **The sidebar** is `tabs::Tabs` in `TabsOrientation::Vertical` — the control
  the Switchboard's System section already uses for exactly this job, turned
  on its side, not a second selection model — standing with the search field
  on one rounded plate drawn with a pane group's recipe. Every row carries its
  own badge and its label, a disclosed pane's included. A category holding
  more than one pane shows a trailing chevron and expands in place; choosing
  it opens or closes that list and goes nowhere, and any number of lists stay
  open at once — the desktop-wide `DisclosureSet` rule. The expanded pane
  rows are rows of the same strip, so one cursor walks the whole column, and
  Right and Left open and close the list under it. The categories fall into
  runs (`registry::Group`), each set apart by a blank break half a row tall.
- **The search field** heads the sidebar's plate (`text::SearchField`) and
  filters the strip to the categories and panes whose label, pane title, or
  *setting* label matches — the index is derived from the one `CATEGORIES`
  table plus each pane's declared setting labels, so a searchable setting
  cannot exist without a row that shows it. Matching a setting selects its
  pane and scrolls that row into view, which is cheap because the registry
  already knows every label.
- **The location band** carries a `nav::Breadcrumb` reading
  `Settings › <category> › <pane>`. Its trailing crumb is the current
  location and is inert; the leading crumbs are the route back, which is what
  makes a narrow window navigable with the sidebar shed. The band is a
  Tab-cycle focus region.
- **Region shedding.** One resolver (`shell::resolve_frame`) resolves the
  band, sidebar, and content once per layout; the paint and the hit test both
  read it, so a press can never land on a control drawn elsewhere. A window
  too narrow to seat the sidebar sheds it — the breadcrumb's leading crumb
  then opens the category list as a `Menu`, exactly the Switchboard's
  section-list idiom — and the content column always survives.
- **The content column** is a vertical stack of `FieldGroup`s under the one
  shared `ScrollBar`. A pane taller than the viewport scrolls; the band and
  sidebar do not.
- **The cursor.** Tab cycles band → sidebar → content → footer. Within the
  sidebar, Up/Down walk categories and panes; within the content, Up/Down walk
  rows and Enter/Space commits the focused row's control. Every control is
  reachable without a pointer, and a refused control refuses the keyboard
  exactly as it refuses the pointer.

### 1.2 Applying a change

Two postures, declared per setting in the registry, never improvised:

- **Immediate** — the change is cheap, reversible, and its effect is the
  feedback: appearance, contrast, density, reduced motion, UI scale, cursor
  set, wallpaper fit, icon flow, notification policy. The row commits on
  interaction; the pane re-reads and shows what took effect. There is no Apply
  button, because there is nothing to batch and a stale Apply is a trap.
- **Staged** — the change is a document a service must validate, or it needs
  re-authentication: the `net.*` and `cache.*` registries, an interface's
  addressing, an account's fields. The pane edits a working copy, shows which
  rows differ from what is in effect, and offers **Apply** and **Revert** in
  the pane footer. Apply posts the whole document (or asks for the one
  elevated run) and reports the outcome in the footer; a refusal leaves the
  working copy intact so the user can correct it rather than retype it.

A pane never mixes the two: a setting is immediate or it is staged, and the
registry says which, so a reader learns the rule once.

---

## 2. The authority map

One row per pane. `read` is what backs the pane's readings; `write` is the
owner the change goes to; the last column is what a refusal looks like.

| Category → pane | Read | Write goes to | On refusal |
|---|---|---|---|
| General → About | `SYSTEM_IDENTITY`, `UPTIME`, `CPU_INFO`, `MEMORY_TOTAL` | — (read-only) | reading renders unmeasured |
| General → Login & startup | ungated `SYSTEM_CONFIG`, parsed by `lib/sysconfig` | elevated `configure` | working copy stands, refusal stated |
| General → Caching | ungated `SYSTEM_CONFIG`, parsed by `lib/sysconfig` | elevated `configure` | working copy stands, refusal stated |
| General → Date & Time | `WallClockReading` | elevated `datetime.app` (launched) | refusal stated, clock untouched |
| Appearance | the session's published settings document | session apply (merged over what it holds) | apply refused, stated on `stderr`, row reverts |
| Wallpaper | session's published settings document; the store catalog and each preview served by the session | session apply (merged) | apply refused, stated; a preview that did not arrive draws its placeholder |
| Theme | — | — (no theme model, §3) | pane states absence |
| Displays | `SEAT_LIST`, `DesktopInfo`, `Compositor::window_scale` | session apply (scale only) | mode change: no interface (§3) |
| Lock Screen | the session's published settings document (`lock.after_min`) | session apply; *Lock Now* is the `LockScreen` window request | apply refused, stated; a refused lock stated on its row |
| Screensaver | the session's published settings document (`screensaver.*`) | session apply | apply refused, stated |
| Power | — | — (no policy interface, §3) | pane states absence |
| Networking → Ethernet | nothing ungated exists: the live readings need `CAP_SYSINFO_HW`/`CAP_SYSINFO_GLOBAL` and stay the Switchboard's, and `network.conf` carries the very identity and addressing those gates protect. The configured addressing is read by the admin-authenticated run (DS8a) | elevated `configure`, which writes the store and hands the changed interfaces to the running stack (DS8) | pane states where the live readings live; a refused apply keeps the working copy and states why |
| Networking → Wi-Fi | — | — | pane states absence (§3) |
| Networking → DNS | ungated `NET_RESOLVER_SERVERS` (the live aggregated set) | elevated `configure` over each interface's own `dns.servers` (DS8) | reading renders unmeasured; a refused apply keeps the working copy and states why |
| Networking → TCP/IP | ungated `SYSTEM_CONFIG`, parsed by `lib/sysconfig` | elevated `configure`, which also hands the policy to the running stack | working copy stands, refusal stated; a stack that did not take it keeps the saved value for next boot and says so |
| Bluetooth | — | — | pane states absence (§3) |
| Sound | — | — | pane states absence (§3) |
| Notifications | the session's published settings document (`notify.*`); the sources that have notified from the session's `QueryNotifySources`, answered to Settings alone | session apply | apply refused, stated |
| Keyboard | the session's published settings document (`key.*`) | session apply (repeat) | layout, remap, shortcuts: no registry (§3), stated on the pane |
| Mouse | the session's published settings document (`pointer.*`) | session apply | apply refused, stated |
| Trackpad | the session's published settings document (`touchpad.*`) | session apply | apply refused, stated |
| Touchscreen | — | — | pane states absence (§3) |
| Printers & Scanners | — | — | pane states absence (§3) |
| Accessibility | the session's published settings document (contrast, density, motion, scale, the pointer's set, size and aids) | session apply (merged) | apply refused, stated on `stderr`, row reverts |
| Language & Region | the bundle `Help/` locale set, `lib/sysconfig` | elevated `configure`; zones → `plans/TIMEZONES.md` | Authority Mark |
| Sharing | — | — | pane states absence (§3) |
| Users & Groups | ungated `USER_DIRECTORY` / `GROUP_DIRECTORY` roster + own record; other accounts' fields, lock state and grants only after admin authentication (DS9) | elevated user-admin tool (DS9) | Authority Mark, account unchanged |
| Storage | ungated `MOUNT_LIST` alone — its `VolumeStats` and its availability overlay; `VOLUME_IO_HEALTH` needs `CAP_SYSINFO_KERNEL` and stays the Switchboard's | — (read-only; mounting is the file manager's) | reading renders unmeasured |

**The one rule behind the table.** Settings never performs a privileged
operation. It renders state, and it hands a typed intent to the process that
holds the authority — the session for the user's own desktop, the console's
broker for anything the machine owns. Nothing in this app can be tricked into
an escalation, because there is no capability in it to escalate with.

---

## 3. What has no interface yet

Each row renders a pane that states the absence in words and names the
prerequisite. None of these is stubbed, faked, or drawn as a control that
would change nothing.

| Pane | What is missing | Prerequisite |
|---|---|---|
| Sound | the stack plays — the `audiod` mixer and router over the first driver — but offers no control over a device's volume or over which device is the default | `plans/SOUND.md` SND15: the device control and the Settings pane over it |
| Theme | nothing names an appearance, a wallpaper and an accent palette together, and the accents are fixed by the light and dark appearances | a new `plans/THEMES.md`: the desktop-theme model (appearance, catalog wallpaper, accent palette) and the palettes it chooses among |
| Bluetooth | no HCI transport, no host stack, no pairing store | a new `plans/BLUETOOTH.md` |
| Printers & Scanners | no print spooler, no scan API, no driver class | a new `plans/PRINTING.md` |
| Touchscreen | nothing to set: a touchscreen covers the one screen edge to edge, and what a touch means is the recogniser's (`plans/POINTING.md`) | a second screen, to choose which one a touchscreen covers |
| Sharing | no SMB, VNC/RDP, or HTTP server in the tree (`userland/net/` is `netstack` alone) | a new `plans/SHARING.md` |
| Networking → Wi-Fi | no 802.11 driver, no supplicant, no scan/associate vocabulary | a new `plans/WIRELESS.md` |
| Displays → resolution, rotation, arrangement | `display_ipc` has `Query`/`Configure`/`Present` only: no mode *list* and no mode *set* | a mode-enumeration and mode-set request in `display_ipc`, plus driver support |
| Power → sleep, battery, thermal | no power-supply, battery, or sensor interface, and no driver to serve one | `plans/DEVICES.md` sensor work + an ACPI/PSCI sleep path |
| Keyboard → layout, modifier remap | exactly one hard-coded US ANSI table (`lib/hid::console`) | a layout registry (`lib/keymap` grows the data; the seat selects) |
| Keyboard → shortcuts | no shortcut registry anywhere; each surface owns its own keys | a desktop-wide binding registry |
| Language & Region → time zone | no zone data, no local rendering | `plans/TIMEZONES.md` |
| General → Software Update | no updater, no package store | out of scope for this plan |
| Accessibility → screen reader, zoom, sticky keys | no assistive-technology surface | out of scope for this plan |

Two of these are cheap enough to build *here* rather than defer, and this plan
builds them because the categories are useless without them: the **idle
interface** the Lock Screen and Screensaver panes need (DS12 — one timer the
session arms only while idle, never a poll), and the **pointer/keyboard policy**
the Mouse and Keyboard panes need for double-click interval, primary-button
swap, and key repeat (DS11 — the session already routes every event, so it is
already the owner). Everything else in the table stays absent and honest.

---

## 4. Controls this surface adds to `lib/controls`

Generic, reusable, and complete on landing — every state, both appearances,
the heavier-contrast and monochrome paths, pointer and keyboard, damage
reporting, and a `widgets.app` gallery tab, exactly as every other family.

- **`form::FieldRow`** — one setting: a leading label, an optional secondary
  description line, and a trailing slot holding one control (a `Toggle`,
  `ComboBox`, `Slider`, `TextField`, `Button`, or a plain read-only value).
  It composes `collection::ListRow`'s row chrome for selection, focus ring,
  and the leading rails rather than restating any of it — but never its hover:
  the row is not what the pointer acts on, so only the slot's control lights
  under it — and it renders
  the three absences of §0 distinctly: plainly disabled, Authority Mark, or a
  stated unmeasured value. Under a narrow width the description truncates
  first, then the label; the control keeps its room, because the control is
  what the reader came for.
- **`form::FieldGroup`** — a captioned plate holding rows, with an optional
  footnote beneath (where a setting needs a sentence of consequence, not a
  tooltip). Rows share one column model so every control in a group lines up,
  and a group draws one plate rather than nesting a plate per row.

DS2 added to that: the one `ComboBox::popup_rect` drop-down placement rule
over the shared `plate_rect` (retiring the three private copies in the widget
gallery, the wallpaper surface and the Switchboard), `FieldGroup::layout` as
its ready-made application for a form owner, and the vertical `Tabs`
sidebar-list anatomy above — a leading glyph, a disclosure chevron, one level
of nesting, and the entry-wise scroll (`set_first`, `seated`) a long list
needs.

Everything else a pane needs already exists:
`Toggle`, `Checkbox`, `Radio`, `ComboBox`, `Slider`, `TextField`, `Button`,
`Tabs` (the sidebar), `SearchField`, `Breadcrumb`, `ScrollBar`, `Menu`,
`Dialog`, `FactList` (read-only panes), `MetricTile` (Storage's capacity
tracks), `StatusPill` (a volume's health band, a link state), and
`ActionRail` where a pane commands a selected subject. **No new control is added for a job an existing one does.**

One `IconKind` per category, each drawn as a **colour badge** — its symbol in
white on a plate of the category's hue, the way macOS draws its settings panes
— so a reader finds a category by colour before reading its name: `Settings`,
`Appearance`, `Wallpaper`, `Theme`, `Display`, `LockScreen`, `Screensaver`,
`Power`, `Networking`, `Bluetooth`, `Sound`, `Notifications`, `Keyboard`,
`Mouse`, `Trackpad`, `Touchscreen`, `Printer`, `Accessibility`, `Language`,
`Sharing`, `Users`, `Storage` (DS15). A pane a category discloses as a row of
its own carries a badge of its own, in its subject's kin rather than its
category's: `About`, `Startup`, `Caching`, `DateTime`, `Ethernet`, `WiFi`,
`Dns`, `TcpIp` (DS17). None of them is the tray reading or the single thing
it stands beside (`Network`, `Volume`, `Bell`, `User`, `Disk`): a category's
symbol is drawn to stand on its badge. The badge is the kind's built-in
picture — compiled in, so the sidebar can never blank and Settings needs no
read or decode authority to show it — and its symbol is the kind's tintable
glyph wherever a control draws only in its own colour. `plans/ICONS.md` I13
carries the badge's own decisions.

---

## 5. Stages

Each stage is one fully-gated increment: it lands with its host tests, its
rustdoc and `docs/` page, and a green whole-workspace validation gate (the
whole `AGENTS.md` §7 sequence, not `cargo xtask ci` alone), and — where the
behaviour is only observable end-to-end — extends the QEMU vertical rather
than a faked run. A stage that turns out larger than one clean increment is
split and staged here, never shipped half-done.

### DS1 — `lib/controls::form`: the form-field family

`FieldRow` and `FieldGroup` per §4, over the existing row chrome, plate,
metrics, and state vocabulary, with a `widgets.app` gallery tab and the
family's specification in `plans/GUI-CONTROLS-DESIGN.md` beside the other
control families. Two pieces of shared machinery land with it, because the
family composes rather than restates: the row chrome `ListRow` and `TableRow`
already share moves out of `lib/controls`' `collection` module into its
`paint` core so all three rows draw one recipe, and `Button`, `ComboBox` and
`Toggle` each gain the measured-width accessor the slot model asks them for —
none carried one, so the slot column had nowhere to come from but a second
copy of each control's own layout arithmetic. A combo's figure is the width of
its *widest* choice, so choosing a different value never moves the column.

The contract it delivers, which no later stage re-derives:

- **Room is given out control, label, description.** The slot is reserved
  first, the label elides into what remains, and the description draws only
  while the label fits whole. A slot never exceeds half the row's content
  (`form::slot_ceiling`), so a label always has room to be read.
- **A row's disposition is the setting's.** `FieldRow::set_state` shares
  enablement, authority and validation — exactly what decides actionability —
  with the control in the slot, so a denied or pending row cannot hold an
  actionable control. A pane therefore states a refusal by setting the
  *row*, never by remembering to set two states in step.
- **The owner places the choice popup.** `FieldGroup::popup_anchor` names the
  row and slot to anchor an expanded `ComboBox` list to; the owner places it,
  hands it back through `FieldLayout::with_popup`, and paints it with
  `render_popup` after every group. The pane (DS2) owns the placement rule,
  because only it knows the viewport the list has to fit in — and DS2 is where
  the one `ComboBox` placement rule is hoisted, retiring the three private
  copies (the widget gallery, the wallpaper surface, the Switchboard's task
  grouping) rather than adding a fourth.

### DS2 — the Settings shell, every category reachable, nothing faked

`userland/apps/settings` (`tairix-settings`) is the bundle: a signed
`AppInfo.toml` declaring `os.tairix.settings`, `kind = "application"`,
`library = "SystemTools"` and `capabilities = ["CAP_CONSOLE_WRITE",
"CAP_SHM"]` with its own SVG master in `Resources/`, a `Help/` tree in every
required locale, a `README.md`, and a `build.rs` mirroring the sibling apps'
`freestanding` cfg so the shell is host-testable and `Run` is a freestanding
program. Its two hand-maintained pins — the harness's discovered-bundle list
and the kernel's capability-request registry — carry it as a
`WINDOWED_APP_REQUEST` application.

What the shell guarantees, which no later stage re-derives:

- **The registry is the surface.** `Category` (22) and `Pane` (28) are closed
  sets and `registry::CATEGORIES` is the single definition of the sidebar
  strip, the search index, the location trail, the keyboard cursor and the
  pane dispatch. Its tests hold totality in both directions, so a category
  cannot exist without a row or a row without a pane, and adding a category is
  adding a row and a renderer.
- **Every pane declares what backs it**, and the two answers are the two
  different facts a reader needs: `PaneBacking::None` names what this system
  does not have and what would have to exist, `PaneBacking::Elsewhere` names
  what the pane will show and where the setting is read or set today. Both
  draw through the one `statement` renderer, quiet and with no plate — the
  shape every other stated absence in the desktop takes. A pane that composes
  no controls declares no setting labels, so a searchable setting cannot exist
  without a row that shows it.
- **`frame::resolve_frame` is the one division of the client.** The paint and
  the hit test both read it. A client narrower than the sidebar plus
  `CONTENT_FLOOR` sheds the sidebar *and* the search field — there being no
  strip left to filter — and the leading crumb then lists the categories as a
  shared `Menu`, placed by the one plate rule. The content column always
  survives.
- **The strip is the vertical `Tabs` sidebar list**, which gained the anatomy
  this needs and `lib/controls` lacked: a leading `IconKind` glyph resolved
  through the owner's artwork lookup, a disclosure chevron stating a
  category's own posture, and one level of nesting for a disclosed pane.
  Twenty-two categories and the breaks between their runs want some 750
  physical pixels at the reference density, so a short window cannot show
  them all: the strip is laid out whole
  and shown through a scrolled view, by pixels, with a gutter of its own —
  carved out of the strip's column, never the pane's — and the cursor, a
  selection and a search result each scroll themselves into view. A category the reader cannot reach is a category they cannot open, so
  this is a correctness property rather than a convenience.
- **The search index is derived, not held.** `strip_rows(open, query)` filters
  the table by a category's label, a pane's title, or a setting label a pane
  declares, folding ASCII case; a category reached by its own label offers
  every pane, one reached through its panes offers exactly those, and a query
  that reaches nothing lists nothing.
- **The launch row** is `SystemAction::Settings` in the taskbar's one
  `system::ROWS` table, at the head of the appearance group, mapped onto the
  bar's existing `TaskbarResponse::LibraryLaunch` and resolved against the
  catalog through the same `installed` predicate *Task Shell* uses — so the
  session gained no new launch path, and an absent bundle renders
  non-actionable with `REASON_NOT_INSTALLED`.

### DS3 — Appearance and Accessibility: the session's user-scope document

The first *writing* panes, and the template for every other user-scope write.
What it guarantees, which no later stage re-derives:

- **One document, two groups.** `lib/wallpaper`'s settings registry — now the
  desktop's, not the pinboard's, and its type renamed `DesktopSettings` to say
  so — grew `appearance`, `contrast`, `density`, `motion` and `scale` beside
  the five backdrop keys, on the same closed-key, tolerant-read,
  canonical-render discipline and the same one format engine.
  `SettingsKey::PINBOARD` and `SettingsKey::APPEARANCE` partition it, and a
  test holds the partition. One document because one owner and one published
  scope: the session writes both in one round trip.
- **An apply merges; it does not replace.** Two surfaces edit the desktop and
  neither shows every setting, so a surface renders only the keys it edits
  (`document_of`) and the session lays them over what it holds (`merge`). The
  old whole-document post would have made every wallpaper change reimpose the
  appearance the chooser happened to open on. A refused document is refused
  whole, on a copy, so nothing half-applies.
- **The axes reach the pixels, and every application.** `Contrast`, `Density`
  and `Motion` moved into `lib/abi::desktop` beside `Appearance` (the ABI owns
  a vocabulary that crosses the window channel; `lib/theme` re-exports rather
  than restating), `DesktopInfo` grew all three, and `adopt_desktop` applies
  them in the one call every app already makes. `ThemeRegistry` gained the
  `Accessibility` overlay — `active()` is the theme *as drawn*, `selected()`
  the theme as registered — and `Theme::with_axes` is the single place an axis
  reaches a pixel. **Density was doing nothing at all** before this stage:
  `Metrics::at_density` now derives the three spacing metrics that decide how
  much room a control is given, so the row is a real setting rather than a
  control that would change nothing.
- **One adopt path, bring-up included.** `PinboardChange` split into
  `BackdropWork` and `AppearanceWork`, and `adopt_appearance` is the one place
  the appearance half is put into effect — re-theme, rescale, republish. The
  session's boot-time settings load drives the very same function, so a stored
  `appearance = dark` is in force before the first frame rather than ignored
  until the user changed something.
- **The panes are one row definition seen twice.** `appearance::Setting` holds
  each settable's label, sentence, choices, read and write; Appearance adds
  light/dark, Accessibility groups the rest the way a reader looking for them
  would, and the registry's `settings` labels are asserted equal to what the
  composition actually draws. `PaneBacking::Composed` is the third answer a
  pane can give, and the statement renderer draws nothing for it.
- **Asked for, never written, and never on the loop.** The apply client
  (`ApplyOutcome`, and `apply` behind `lib/wallpaper`'s `rt` feature) is one
  definition shared with the chooser, and Settings drives it from a worker:
  the session answers only once its store has been written, so an inline call
  would freeze the window for a disk commit. The rows show the choice at once
  and adopt the durable value when the answer lands, so a refusal reverts.

**The cursor pair is DS3b's, and it landed there.** DS3 left Accessibility
stating that the desktop kept no pointer size; DS3b replaced that statement
with the POINTER group's two real rows.

### DS3b — the cursor pair

`cursor.set` and `cursor.size` joined `SettingsKey::APPEARANCE` and the
Accessibility pane's POINTER group. What it guarantees, which no later stage
re-derives:

- **The pointer is rasterised to a pixel *side*, not to a factor of its own
  design grid.** `VectorCursor::rasterise(side)` replaced
  `rasterise(scale_percent)`/`footprint`, because the grid is an authoring
  detail — 32 units for a built-in cursor, `tairix_svg::DESIGN_GRID` for a
  decoded one — so a factor gave a different pointer size per set and
  swapping sets would have resized the pointer. **That was a live defect, not
  a refactor:** the first shipped SVG set would have drawn a 2048-pixel
  arrow. `CURSOR_BASE_SIDE_PX` is the logical reference side, and
  `Scale::scale_length` is still the one logical-to-physical conversion.
- **The cache epoch is the pixel side and the set, not the scale, the size
  and the set.** An image depends on how many pixels across it is and which
  set it came from and nothing else, so two (scale, size) pairs resolving to
  one side correctly share one cached image. The controller owns the pointer's
  *logical* side (the user's own choice) and never the scale (the output's).
- **`CursorSetId` is an owned bounded name**, `tairix_theme`'s — held inline
  (`lib/inline`'s `ArrayString`) so the epoch stays `Copy` and no allocation
  reaches the compositing path, and validated as a plain leaf name within
  `tairix_abi::desktop::CURSOR_SET_NAME_MAX` because it is spliced into a
  store path. The name *is* the chooser's label, verbatim, as a wallpaper
  category's is. It lives in `lib/theme` — the cursor *vocabulary* crate,
  whose only dependencies are themselves dependency-free — rather than in
  `lib/cursor`, so the settings registry can hold one without linking a
  rasteriser.
- **The store is `<set>/<asset-id>.svg`, a categorised graphics family.**
  `GraphicsFamilyKind::Cursor` is the third family, discovered from
  `lib/cursor/assets/` by the same `GRAPHICS_FAMILIES` walk, and validated as
  it is discovered: an asset name no kind asks for, a set directory no chooser
  could offer, an over-large file, or two files claiming one kind fails the
  *build*. `tools/xtask` additionally decodes and rasterises every shipped
  asset, so artwork that draws nothing or puts its hotspot outside itself is a
  build error rather than a broken pointer.
- **One set ships: `High Visibility`** — a dark pointer under a wide white
  halo, its own bolder geometry rather than a recolour of the built-in
  tables. With the always-present built-in `Standard` that is a choice of
  two, which is what makes the row a control rather than a list of one.
- **Every set is loaded at bring-up; activating one reads nothing.** The
  session walks the store once (`/System` is read-only, so the choice space
  is fixed for the boot) and registers all of them. A settings apply arrives
  on the loop that owes the user a frame, so activation had to be pure memory
  — the alternative was a directory read on that loop, or a whole second
  worker desk for nine small files.
- **A stale set is a legal value.** `cursor.set` names a set; it does not
  assert one exists. A stored choice outlives the image that shipped it, so a
  set an update removed still parses (the rest of the document survives),
  is still *offered* under its own name (opening the pane changes nothing),
  and falls back to the built-in artwork at activation.
- **A third served request, of `QueryDesktop`'s posture.** `QueryCursorSets`
  is seat-scoped, capability-free, and a read: Settings holds no
  `CAP_FS_ACCESS`, so the session lists and Settings asks, exactly as for the
  wallpaper catalog. Unlike `QueryWallpapers` it carries **no operand and no
  paging**: at most `CURSOR_SETS_MAX` short names, which one reply frame
  holds outright, so there is no paging to get wrong.
- **`AppearanceWork` gained a `cursor` flag.** The pair moves the pointer and
  nothing else: a re-theme would repaint every surface and a rescale would
  move every length, for a change only the pointer can see.

### DS4 — Wallpaper: the gallery absorbed, and `wallpaper.app` deleted

Wallpaper is a *section of Settings*, not an application beside it. The
picture gallery is in the Wallpaper pane, `userland/apps/wallpaper` is gone,
and the backdrop menu's *Change Background…* row opens Settings at that
pane.

**Settings still holds no domain authority, and the gallery did not change
that.** Listing the shipped store needs `CAP_FS_ACCESS` and decoding an
untrusted picture needs the sandbox worker a `CAP_PROC_SPAWN` holder hosts —
which is exactly why the old chooser held them. Granting all three to the
application that will later carry Networking, Users and Storage is the
ambient-authority god-app §0 exists to prevent, so the gallery is served
rather than hosted: the desktop session already owns a sandboxed image
renderer, and Settings asks it.

- **Two descriptive window-channel requests** (the render generalised by
  DS19), of the same posture as `QueryDesktop`: seat-scoped, capability-free,
  describing the caller's own desktop and granting nothing. Both are *reads*, so §0's "three write paths
  and no fourth" still holds — the only write is the existing session apply.
  - `QueryWallpapers { from }` answers a **page** of the flat catalog, with
    the catalog's total so a caller knows whether to ask again. The session
    walks the store **once, at its own bring-up** — `/System` is read-only,
    so the catalog is fixed for the life of the boot — through
    `catalog_categories`, `catalog_entries` and `lib/wallpaper`'s
    `desktop_catalog`, and answers from memory. That is what keeps a
    directory walk off the compositing loop while leaving the query as
    ordinary and synchronous as `QueryDesktop`. Paging exists because the
    channel's reply frame is bounded; the bound on the catalog itself is
    `MAX_WALLPAPER_CATALOG_ENTRIES`, applied to the whole flat list because
    that bound is a gallery's rather than one directory's.
  - `RenderPreview { window_id, shm_handle, subject, width, height }`
    renders one picture into a region **Settings** created and granted, which
    is the one thing its `CAP_SHM` already lets it do. Its subject is a
    **catalog position** or a **screensaver kind**, never a path, so it cannot
    make the session read a file the caller chose; its size is bounded by
    `WINDOW_PREVIEW_MAX_SIDE`, which a chooser's picture at the widest scale
    fits. Its reply is only the acceptance — the read and the sandboxed decode
    happen on the session's existing wallpaper worker, off its loop — and it
    concludes with a `WindowEvent::PreviewRendered` echoing the subject and
    the size, so an answer cannot be adopted for the wrong picture. There is one sandboxed decode path on the desktop instead of
    two, and no picture is decoded in the address space of the application
    that browses them.
- **Previews across the cores, and the backdrop first.** The session's
  `WallpaperDesk` takes its own backdrop before any preview, so the picture
  the user is looking at never waits behind a thumbnail, and renders
  previews on one preparer per online CPU within a memory budget: a render
  reserves its preparation, then acquires what its worker's plan reports or
  waits first in the queue until that fits, one render always going on
  (`plans/PINBOARD.md` §8). No window holds more pending than there are
  preparers: the bound on how much decoding one client can set going. **A closed window
  costs at most what is under way:** a render a preparer has taken is refused
  its memory, never queued again, and answers into nothing, freeing its slot;
  every close is
  recorded in the session's window table, and before its next park the serve
  loop withdraws that window's waiting previews from the desk and lets go of
  its regions, so reopening windows can neither queue decodes ahead of
  another client's nor pin regions in the desktop. The loop holds the
  *mapping* and the desk holds the request.
- **The pane** is an ordinary form of two groups: the four pinboard settings
  (fit, backdrop, icon flow, sort) from the same `Setting` registry as
  Appearance's, then the *Desktop Picture* chooser (DS19), both posting
  `SettingsKey::PINBOARD` through DS3's merge, so they cannot disturb the
  appearance keys. A `Composition` names the key group it renders, which is
  what makes that true of every pane rather than of this one.
- **A picture the catalog does not hold** — one in effect before it was
  removed from the store, or set from outside it — is still offered and still
  selectable. It has no catalog position, so it cannot be rendered and draws
  its glyph and its name. That is the one capability lost with the chooser's
  `CAP_FS_ACCESS`, and it costs a thumbnail rather than a choice.
- **`userland/apps/wallpaper` is deleted**, with its bundle, manifest,
  resources, `Help/` tree in every locale, and README. The candidate model,
  the "no picture" entry and the backdrop palette are re-expressed in the
  pane; the gallery's wrapping and hit-test geometry was already
  `lib/browse`'s and is simply composed. Every reference went in the same
  change: the harness's discovered-bundle list, the kernel's
  capability-request registry, the QEMU fixtures, `SETTINGS_RUN_PATH` /
  `SETTINGS_LABEL` replacing `WALLPAPER_*` in the session, `PLAN.md`,
  `plans/PINBOARD.md`, `docs/src/desktop/pinboard.md`,
  `docs/src/lib/wallpaper.md`, and the §15.18 jump-sheet. The `Help/` trees
  and the bundle set are build-discovered, so both followed the directory.
- **A launch may name a place inside an application.** *Change Background…*
  goes through the desktop's one launch funnel with `LaunchTarget::Pane`. It
  is a **third** target form rather than a `LaunchTarget::Path`, because the
  two existing forms are both authority over a *file*: a path is resolved
  under the application's own authority, which Settings has none of, and a
  document is a one-shot delegation. A pane is neither — a name resolved
  against the closed pane registry, conferring nothing, and an unknown one
  leaves the window on the pane it already showed. Singleton is the signed
  manifest's default, so a running Settings is handed the pane and navigates;
  a fresh one is given the same pane as its one argument and opens on it.
  Each pane row carries a stable `name` for this, separate from its `title`
  because a title is what a reader reads and may be reworded.
- **The second form idiom DS14 tracks lost one of its three instances here**,
  by the surface carrying it ceasing to exist.

Host tests: the pane names being unique and resolvable and an unknown one
resolving to nothing; the session desk's backdrop-first, one-at-a-time and
self-freeing rules; and the window channel's catalog page and render
accept/refuse paths. The chooser's own are DS19's.

### DS5a — the shared volume view model

The mount record → capacity/health derivation lives in one home every
surface reads, rather than in the Switchboard's private `resource_report`.

- **`lib/procinfo::volume`** owns the reading→facts conversions: `VolumeBytes`
  (`total`/`free`/`available`, with `used`, `usable`, `used_permille`, and a
  saturating `plus` fold), the two availability spellings
  (`availability_marker` for a `mount(8)` listing, `availability_name` for a
  fact list), `medium_name`, and `volume_health_name`.
- **`lib/abi`** carries the banding beside the state it bands:
  `MountAvailability::health` → `VolumeHealth` (`Healthy`/`Degraded`/
  `Failing`). Derived, never transmitted, and monotone in the existing
  `severity` ranking, so folding a stack with `worse_of` and banding agrees
  with banding each layer and taking the worst.
- **`lib/theme`** carries the one band→`SignalRole` binding
  (`SignalRole::for_volume_health`), because it owns the role vocabulary and
  a CLI tool must not link a theme to print a mount table.

**Two shares, both named, neither the other's default.** `used_permille` is
of the whole medium — what a capacity bar means — while `df`'s GNU `Use%` is
of `usable()`, what a caller may actually allocate. A withheld reserve is
unallocated to both numerators but part of the medium to only the first, so
`df` reads higher on a reserved format; it keeps the GNU definition it is
bound to and reads it off the same model.

Its callers: the Switchboard's storage pane (which bands health through
the shared `VolumeHealth` alone, and whose capacity block names its rows
`Capacity` and `Available` because the two figures differ), `sysmon`'s mount
panel, `fstree`'s two volume walks, `stress`'s scratch-space probe, and
`df`'s `byte_figures`. None keeps a copy, and `fstree`'s own `VolumeSpace`
is gone: it held the same two facts.

### DS5 — Storage

The per-medium used-space overview, in `userland/apps/settings/src/volumes.rs`:
one **card** per mount walked from `MOUNT_LIST` through
`lib/procinfo::for_each_mount`. A card is a `FieldGroup` captioned with the
volume's name (`mount_name_bytes` — its source, else its mount point), with
the banded `VolumeHealth` as a `StatusPill` toned through
`SignalRole::for_volume_health` as the group's **badge**
(`FieldGroup::with_badge`, added with this stage — the group places it,
because it is the only thing that can also take the room out of the caption
and out of the band's height, so a long volume name is cut rather than drawn
under the capsule), rows for mount point / filesystem / device /
medium / availability, and beneath it a `MetricTile` whose
`MetricInstrument::Track` is `VolumeBytes::used_permille`. Both plates are one
scrolled unit, so a capacity can never be on screen without its volume.
Read-only: mounting and unmounting are the file manager's and `mount`'s, and a
second route to them is duplication.

**A volume that reports no capacity has no tile at all** — the card carries a
`Capacity` row of `FieldControl::Unmeasured` instead, so the in-RAM layout
mounts never draw a full bar or an invented percentage. A field the table left
empty is likewise `Unmeasured`, never a blank a reader would take for a
reading. The row labels are one list (`VOLUME_FACTS`), read by the registry's
search index and drawn by the card, so a term that reaches the pane reaches a
row it shows.

**The health pill is the mount table's own availability, not the gated
counters.** Settings holds neither `CAP_SYSINFO_KERNEL` nor
`CAP_SYSINFO_GLOBAL`, so `VOLUME_IO_HEALTH`'s bucketed completions are the
Switchboard's alone. It does not need them: `MOUNT_LIST` is ungated and its
record already carries the live availability overlay a failing or recovering
device sets. The manifest stays `CAP_CONSOLE_WRITE` + `CAP_SHM`. The band is a
summary, so the *exact* state stays on its own row — "recovering" and
"degraded" must not collapse into one word.

**The mount walk is an IPC round trip, so it never runs on the loop that owes
a frame.** It is read at bring-up beside the picture catalog and the cursor
sets, and re-read through a second `tairix_rt::work::Worker` desk on its own
wait-set token (`MOUNTS_TOKEN`) — submitted, never awaited. A second *desk*,
not a second deferral scheme: the applier's is latest-wins over one job slot,
so sharing it would let an apply and a walk evict each other. `Shell` says it
wants one (`volumes_wanted`) when the pane *comes* on show and the `Run` binary
submits; the answer lands as an ordinary wake. Leaving and returning asks
afresh, because unlike the shipped picture store the mount table moves.

**The three body shapes, and why the shell no longer spells two.** The pane
column was `Option<Form>` + `Option<Gallery>`, matched as a tuple at every
measure/render/scroll/input site, with `(Some(gallery), None)` a state that
could not occur but had to be handled. It is now one `body::Body` enum —
`Statement`, `Form`, `Pictures { form, gallery }`, `Volumes` — so the
impossible pairing is unrepresentable and the storage body got a deliberate
arm at each site rather than falling through a form's. Three predicates carry
what the sites used to re-derive: `composes_controls` (is the column on the
focus ring in its own right), `column_scrolls` (every body but a gallery
beneath a fixed form), and `is_listing`. The plate-stacking arithmetic `Form`
carried is `stack::{place, gap, height}`, shared with the volume cards,
because a second copy of "place plates down a column" is the duplication the
charter forbids.

**Every column scrolls by pixels.** The strip and the pane lay their content
out whole, unscrolled, and show it through a `ScrollView`: a row or a plate the
edge crosses is drawn cut and still answers where it shows. A line step is a
control's height at the desktop's density (`body::line_step`), the gallery's a
line of tiles, a page the viewport less a line, and a wheel detent the shared
`WHEEL_STEP`; a scroll repaints its bar with the rows it slid, and a wheel over
either bar scrolls it. A keyboard walk reveals the cursor's whole group with
the gap that frames it where that fits, else its row with the caption above a
first row or the footnote below a last one, else the row alone.

**The pointer's routing.** An open choice list is painted above the footer
band and the bars it hangs across, and holds the pointer — ahead of the band,
the bars and the gallery — until it resolves; a press on it reaches no row
beneath it in any group. A move reaches the region it leaves as well as the one
it enters, so nothing stays lit for a pointer that is elsewhere, and a round
that repainted anything is presented. The keyboard cursor follows a press,
never a hover. What is lit follows the pointer rather than the content: a round
that scrolled a column or laid it out afresh, and every `lay_out` a caller
drives after adopting an answer or a resize, replays the resting pointer as a
move through the same routing, so a control, a plate or a tile the content
carried away gives its hover to whatever now lies under the pointer. The replay is a
move — it presses nothing and takes no cursor — and a window that has not yet
seen the pointer replays nothing.

**This must not become a second Storage page.** The Switchboard's System
section already has one, and the two answer different questions — *how full is
each medium* here, *is each volume healthy and how hard is it working* there.
They share facts, and DS5a is where those facts are derived; neither surface
keeps a copy. Two further shared pieces landed with this stage for the same
reason: `lib/procinfo::mount_name_bytes` (the source-else-mount-point naming
rule both surfaces use) and `lib/util::size::{binary_scale, format_at_scale,
format_binary}` (the desktop's prose byte ladder, hoisted out of the
Switchboard's private `format` module so a capacity reads the same on both).
That ladder reaches `EiB`: a byte count is a `u64` throughout the ABI, and a
rung short spells the top of its own domain as four figures of the rung below.

### DS6 — the elevated-apply seam, and General

`ElevateRequest::Run` carries a bounded argv — at most
`ELEVATE_MAX_ARGS` (16) arguments, `ELEVATE_MAX_ARG_LEN` (512) bytes each,
`ELEVATE_MAX_ARGV_BYTES` (1024) in all, one admissibility rule shared by the
encoder and the decoder — so a caller can run the tool that already owns a
store with the one change a user asked for instead of that store growing a
second writer. It widens no authority: the request already named an arbitrary
absolute program, and the broker keeps every check it had. A malformed or
over-long vector is refused at the decode, *before* an attempt is spent
against the named account, and the audit records the argument **count** and
never the arguments — the broker hands them over without interpreting them,
so it cannot know which is a secret. The shell's `elevate` builtin grew the
same operands, so the CLI and the GUI reach the writer identically.

**Reading the machine's store.** Settings holds no `CAP_FS_ACCESS` and never
will, so `system.conf` is served by a new ungated `sysinfo-v1` query,
`SYSTEM_CONFIG`, over a kernel introspect domain of the same name: the kernel
already owns the VFS and already parses this document at the root unlock, so
serving it adds no authority to anything and no manifest widens. It answers
the **text**, read fresh (a boot snapshot would report the old value after
`configure` wrote a new one), and `lib/procinfo::system_config` parses it with
`lib/sysconfig` — the engine `configure` writes through. Ungated on the same
ground as `MOUNT_LIST`: a world-readable public document carrying no
credential, with no write path anywhere near it. The `network.conf` read is
not the same shape: that document is not world-readable, which is why the
Ethernet pane is answered by an authenticated run instead (DS8a).

**General.** *About* and *Date & Time* are read-only fact columns — one
label-and-reading row per figure, each from its own ungated query, so one
refusal costs one row and every reading that did not arrive says so. *Login &
startup* and *Caching* are **staged**: a choice edits a working copy, the
pane's action band says how many rows differ, and Apply asks for an account
once and runs `configure` once with every changed key. That made `configure`
accept several `<key> <value>` pairs, applied to one rendered document, so a
group of settings can never be left half written — the defect a run per key
would have had. The master switch's ceiling is shown as the ceiling it is: a
per-class row keeps its own value, because that is what the store says, and
states that caching is off for the machine so it cannot be read as running.
Date & Time launches `datetime.app` through the broker's existing `Launch`,
started and left running.

**The credential question is shared.** The desktop has one credential
surface, `lib/controls::CredentialSheet`: the session's prompt window and this
application's in-window sheet compose the same focus order, wording, "an empty
field is never offered" rule and secret hygiene. It is modal while it is up.

**One more desk each way.** The store read and the elevated run are worker
desks like DS5's, because the broker answers only once the program it started
has exited — a window that waited would stop drawing for the whole of an
authentication and a store write.

### DS7 — Networking: read, and the stack-wide options

Two composed panes, and the authority line between them and the third is the
point of the stage.

**TCP/IP** is the six stack-wide `net.*` keys (IPv4/IPv6 enable, IPv6 privacy
addresses, SYN cookies, keepalive, ECN) as six more `MachineSetting`s in a
third `Composition` — the same staged posture, the same ungated
`SYSTEM_CONFIG` read, and the same single elevated `configure` run as
Login & startup and Caching, with no new form machinery and no second writer.
It applies **live**: `configure` already hands a changed `net.*` policy to the
running stack over `CAP_NET_ADMIN` after writing the store, and reports a
refusal there as a saved-but-not-applied notice rather than a success. The
ceiling rule DS6 established for caching generalises here — the
temporary-address row states that IPv6 is off, and the three connection rows
state that a machine with neither family makes no connections at all — with
one shared mechanism rather than a second special case.

**DNS** states the live aggregated resolver set from the ungated
`NET_RESOLVER_SERVERS` query: one row per server, discovered rather than
declared, re-read when the pane comes on show because leases move. An empty
set ("this machine resolves no names") and an unavailable reading ("not
measured") stay distinct facts.

**Ethernet takes no *ungated* reading, and that is the finding of this
stage.** The plan originally had it read
`NET_INTERFACE_FACTS`/`_STATE`/`_RATES`; those need `CAP_SYSINFO_HW` and
`CAP_SYSINFO_GLOBAL`, which §0 says this application never holds, so every
row would have been refused on every machine for ever — a dead row, not a
denied action. Nor can `network.conf` be the ungated way round: it carries
the `match.mac` hardware identity and the static addressing those two gates
exist to protect, so serving it ungated would defeat them. The live readings
stay the Switchboard's, exactly as `VOLUME_IO_HEALTH` does for Storage; DS8a
gives the pane the *configured* addressing instead, answered by an
administrator-authenticated run. Wi-Fi keeps its §3 absence.

**One shared address spelling.** Rendering an address was duplicated three
ways and two of them disagreed — `lib/procinfo` printed RFC 5952 canonical
IPv6 while the Switchboard wrote all eight groups uncompressed, so one machine
spelled one address two ways. `lib/procinfo::netaddr` is now the single
definition (`render_ip`, `render_server`, `render_if_addr`), RFC 5952
throughout, and every surface reads it.

### DS8a — the elevated-read seam

This is the prerequisite DS8b and DS9 both build on.
`ElevateReply::Completed` carried an exit code and nothing else, so no
authenticated run could *show* a caller anything.

`ElevateRequest::Capture` is that form: the identical re-authentication,
signed load gate, run-as-uid and audit as `Run`, but the `Run` binary binds
the child's `stdout` to a pipe it owns (`pipe_create` + a `SpawnAttach`
handle wire), drains it to end of stream **before** reaping — a child that
fills the pipe blocks until it is emptied, so the other order hangs — and
answers `ElevateReply::Captured { exit_code, output }`. The child's `stdin`
and `stdinfo` are closed (a run nobody can see is not prompting) and its
`stderr` stays login's console. A separate form rather than a flag on `Run`:
relaying a program's output to an unprivileged caller is a new information
flow and is visible as one at the call site.

**What a caller can induce a named program to print, answered.** The caller
chooses both the program and the argv, so the relayed bytes are
attacker-influenced by construction — but the form widens no authority. The
caller must still offer the target account's password, and an account that
re-authenticates could already be given a *shell* as that account through
`Launch`. What the seam changes is only that the bytes come back as data
instead of onto a console the caller shares; behind a desktop that console is
invisible, which is the whole point. The bound is therefore on the **volume**
of relayed bytes, not on their secrecy: `ELEVATE_MAX_OUTPUT` (4 KiB) is a
fixed containment bound, sized from the widest listing the consumer asks for
(a `configure` listing of both registries, whose per-interface lines run to
roughly a kilobyte for a fully specified interface). A run that prints more
is answered `Overran` with **no** bytes at all — never a prefix a caller
could mistake for the whole — and the audit records that output was returned
and how much, never what it was.

**Its first consumer, and the read half of the network registry.**
`configure` resolves a key name against the flat `lib/sysconfig` registry
first and then against the per-interface `<iface>.<suffix>` registry of
`lib/netconfig`, so a listing states both and a `Show` reads either. The flat
registry wins, so an interface alias can never take a machine setting's name
over, and a test pins the two name sets disjoint over both `ALL` arrays
rather than resting on today's accident that none collides. The alias grammar
is `lib/netconfig`'s one `valid_iface_name`, not a second copy. That registry
has no defaults, so a listing shows only what the document holds and a `Show`
of an unset key answers an empty line rather than inventing a value.

Settings' Ethernet pane is then a composed reading: it opens stating that
nothing has been read and offering *Show Addressing…*, the reader offers an
account, and the supervisor runs `configure` as it and relays the listing.
Each line is read back through the same `IfaceKey` registry, so the machine
settings in the same listing are dropped and only `<iface>.<suffix>` lines
become rows — one plate per interface, labelled in a reader's words. An
overrun states that it was too large and shows no part of it; a refusal
states the refusal and leaves the pane saying nothing was read.

### DS8 — the network store's writer

`configure` could resolve and show a `<iface>.<suffix>` key but
could not set one; the store had a parser and a render and no mutation API
at all.

**`lib/netconfig` grows a draft.** `NetworkConfig::edit()` yields a
`ConfigDraft` whose `set`/`unset` accumulate and whose `commit` checks the
result once. A draft rather than a mutating `set` on the configuration
itself, because a whole-document invariant cannot be checked a key at a
time: moving an interface from a static address to DHCP has to drop
`ipv4.address` and change `ipv4.method` together, and there is **no ordering
in which each half alone is a document the parser would accept**. Until the
commit the configuration the draft came from is untouched, so a refusal
leaves no partial change anywhere, and a committed `NetworkConfig` is always
one the parser accepts back. `commit` also refuses a document whose render
would outgrow `MAX_CONFIG_LEN` — `MAX_INTERFACES` fully specified interfaces
render past it, and a store the writer had just written would then be
refused by the next reader — and drops an interface left declaring no key,
which would write no line and so break the round trip.

**The *unset* spelling is the empty value, and it is genuinely required.**
That registry has no defaults, so a key is removed rather than reset, and
`validate` refuses a non-static method carrying a static address: without a
removal an interface in `static` could never be moved to `dhcp` at all. No
key in the registry accepts an empty value — a test pins that over the whole
of `IfaceKey::ALL` — which is what makes the spelling unambiguous, and
`ElevateArgv` carries an empty argument as typed.

**One projection, two pushers.** `InterfaceConfigPlan` and its mapping from
the document to the `netstack-v1` messages moved out of the device manager
into `lib/netconfig` (`InterfaceConfigPlan::of`). `configure` pushes what a
live edit changed and the device manager delivers the same plan at boot, so
a second copy of "what this setting means to the running stack" cannot
exist. `configure` compares the plan either side of the edit and pushes only
the interfaces that actually differ, so an interface the command line did
not name is not reconfigured — and a member whose bond took it over is,
because its message changed even though its own keys did not.

**Two limits are reported rather than hidden** (`AGENTS.md` §2.24). The
stack's admin surface carries no message that *retires* an interface, so one
removed from the document keeps running until the next boot and the tool
says so. An interface saved with neither `match.mac` nor `match.node` can
never be bound to a device, so it is saved and the refusal stated.

**Devmgr's static-only caching is retired.** `deliver_interface_configs`
read the plan once (`if state.plan.is_none()`) and cached it, so a runtime
edit was never seen. It now re-reads on every generation bump, exactly as
the stack-wide policy does, and forgets a delivery mark only for an
interface whose message changed — so an edit reaches the stack while an
untouched interface is not re-pushed. An unreadable store leaves the plan
already held standing rather than wiping it.

**`ValueShape` is now the shared configuration vocabulary**
(`tairix_util::conf`), stated by both registries: `IfaceKey::shape()` is
what lets a refusal name the valid choices whichever registry refused, and
what a settings surface will build its combo rows from rather than a second
copy of the value sets. Each closed enum's `VALUES` is derived from its own
`ALL` and `as_str`, so the set a chooser offers and the set the parser
admits cannot drift.

### DS8b — Ethernet and DNS stage and apply

DS8a gave the Ethernet pane a *reading* of the configured
addressing and DS8 gave the system a writer; what remained was the pane that
stages a change over that reading and applies it.

**Both networking panes are compositions now, not fact columns.** `Ethernet`
and `Dns` are `Composition`s of `Posture::Staged`, so `PaneContent` lost its
two read-only variants and `Facts` is the About/Date & Time pair alone. The
DNS pane's live resolver plate is a group of reading rows *inside* the form,
above the per-interface ones, rather than a second body shape: a form of
readings is what the family is for, and `Facts + Form` would have been a
fifth body to lay out and scroll.

**Groups discovered, rows owned by an index.** A networking composition
declares no `GroupSpec`: `network::interface_groups` builds one plate per
interface of the **captured** document. `Owner` grew `Interface(IfaceSetting
{ iface, key })`, naming the interface by its index there rather than by an
owned alias, because the owner table beside a form's rows is `Copy` and sits
alongside the two static stores' settables. The plates come from the capture
and not from the working copy, so clearing an interface's last key cannot
make its plate — and the rows the reader is typing in — vanish mid-edit.

**The working copy is the reader's edits, not an edited document.** This is
the load-bearing decision. `ConfigDraft` checks a document whole because
neither half of "drop `ipv4.address`" and "set `ipv4.method dhcp`" is a
document the parser accepts, so a working `NetworkConfig` kept valid after
every keystroke could never reach the change the pane exists to make. The
form therefore holds `Vec<(IfaceSetting, String)>` — the changed keys and
what each now says, the empty value being the registry's *remove* — and:

- each value is checked against **its own key** as it is typed, through
  `IfaceKey::admits` (new in `lib/netconfig`: `set_key` on a throwaway
  interface, so a surface asks the parser rather than a second grammar).
  A refused value marks the row `ValidationState::Invalid`, keeps exactly
  what was typed, and blocks Apply — it is never dropped from the change;
- the **whole** document is checked once, by `Form::proposal` (the capture's
  own `edit()` + every staged pair + `commit()`), *before* a password is
  asked for. An inconsistent document is refused in the band naming what is
  wrong, rather than by a run the reader has just authenticated.

`Form::pending` answers owned `(key, value)` pairs, which is exactly the argv
`configure` takes, and a machine row's pair is spelled the same way.

**The state machine, and the capture's lifetime.** Before a capture the band
is the single **Show Addressing…** command; a landed capture makes it
Revert + Apply. Leaving the pane drops the capture (`restate_body` clears it
when the *location* is not a networking pane, before the next body is built),
so a privileged reading never sits in this application while the reader is
elsewhere — and moving between Ethernet and DNS keeps it, because both are
discovered from the same document.

**An applied change is recorded, not re-read and not forgotten.** The network
store cannot be re-read without a second password, so a clean exit adopts the
proposal as what is now in effect. That is an acknowledgement, not a reading:
`configure` applies every named pair or none, and both sides render through
the same engine. The pane claims only the keys it named, and the dropped
capture is how a reader gets the document as it now stands.

**Which keys are settable.** The addressing rows (`ipv4.*`, `ipv6.*`, `mtu`,
`dns.servers`); `kind`, the two `match.*` keys and the `bond.*` keys stay
readings — hardware identity and bond composition are not a settings-pane
job. `dns.servers` is offered on both panes, from the one definition, exactly
as Contrast is offered on Appearance and Accessibility. A closed key's
choices come from `IfaceKey::shape()` with a leading *not set* entry, which
is the one thing only the document can express. An unset **settable** still
draws its control (an interface on DHCP must be reachable to give a static
address to); an unset **reading** draws no row, because there is nothing to
read.

**Two defects this stage owns and fixed** (`AGENTS.md` §2.18). `Form::is_dirty`
was dead public API and the per-row "which rows differ" the staged posture
promises was never drawn: each plate now carries a `StatusPill` badge saying
how many of its rows are staged, set in place (`FieldGroup::set_badge`, new)
so a row holding a caret survives its plate learning it has changed, with the
column re-measured only when a badge actually appeared or went. And the
action band offered every reading pane an enabled **Revert** that did
nothing, and said "0 changes not applied" beneath it: a band is now either
`Footer::staged()` (Revert + Apply) or `Footer::command(label)` (one
command), each button enabled by its own rule, and a command band says
nothing until it has something to report and stays usable after it has been
used.

### DS9 — Users & Groups

The pane composes its three reads, stages a per-account change, and
applies it as one elevated run; the ledger above records what that now
guarantees.

**The read.** The whole `users_admin` syscall is gated on `CAP_USER_ADMIN`, and
the only other account read — `users_db_read` under `CAP_USERS_READ` — answers
the credential database itself. Settings takes neither.

`USER_DIRECTORY` answers uid and username alone, which is not a Users pane. The
pane is therefore built from three reads of *different* authority, rather than
from one ungated query widened until it is enough:

- **The roster** — every account's uid and username — is the existing ungated
  `USER_DIRECTORY`, walked paged. A sibling ungated `GROUP_DIRECTORY` answers
  gid and group name on the same ground and nothing else: rendering a gid is
  the same display need as rendering a uid.
- **The caller's own account** — full name, shell, home, primary gid, group
  memberships — is read for the uid the kernel attests, ungated, because a
  principal reading its own record crosses no boundary.
- **Any other account's fields, every account's lock state, and the grant
  ceiling** are answered by the administrator-authenticated run the write path
  already performs, and are shown only after it. This needs no new interface:
  `users_admin` already carries `ListUsers` and `ListGroups` behind
  `CAP_USER_ADMIN`, so the gated half of the pane is a read the elevated tool
  can already serve.

The third line is the one to be explicit about, because widening the directory
is the tempting shortcut and it is a real loss. An ungated lock state is an
enumeration of which accounts are live and so worth attacking; an ungated shell
and home path are reconnaissance any unprivileged process — a compromised
parser sandbox included — learns nothing of today. None of them is a display
pairing, so none rides the directory's justification, and TAIRiX has no
world-readable `passwd` file to inherit the habit from. The default is closed: a
field enters the ungated directory only where the DS9 review positively shows a
name cannot be rendered without it, and the burden is on the field. Password
records stay behind `CAP_USERS_READ`, the grant ceiling behind
`CAP_USER_ADMIN` — it is a map of the machine's authority, not directory data —
and no new capability is added for any of this.

Both directory frames are fixed-width, fail closed on decode, and enter the
`lib/abi` fuzz seed like every other frame; the roster is walked paged, so a
machine with many thousands of accounts leaves no whole-set copy resident.

**The write.** The user's own full name and password are changed through the
broker's re-authentication of that same account. Administering another account
— create, modify, lock/unlock, delete, set grants, groups — elevates the tool
that owns the syscall, which means the user-admin command family grows the
operations `users_admin` already carries and no tool yet spells (modify,
delete, lock/unlock, set grants, set password, delete group). Settings
reimplements none of them, and holds no path to any of them without a password.
A grant the authenticated account may not confer is refused by the kernel and
stated; the pane never pre-approves an escalation, and the kernel's
never-widen and last-administrator rules remain the only arbiters.

### DS10 — Notifications

What it guarantees:

- **One gate, at the one intake.** `serve_notify` attributes every notice to
  the bundle the kernel attests its producer runs (`Origin::app`), and holds
  it to `DesktopSettings::notifications` there and nowhere else. A producer
  running no verified bundle has no name a policy could hold and is refused
  `PermissionDenied`. A refused raise is never delivered, drawn or recorded as
  shown, withdraws what the same key showed before, and is answered as
  accepted; a clear always applies; a changed policy withdraws what it no
  longer admits (`PinboardChange::notifications`).
- **The policy is two keys.** `notify.enabled` is the desktop-wide switch and
  `notify.sources` one `<bundle>:<level>` entry per source that does not show
  everything (`lib/wallpaper::notify`, levels *all* / *warning* / *critical*
  / *none*). A bundle identifier has more segments than a key may, so the map
  is one value rather than a key family; `NotifyPolicy::set_level` refuses a
  change whose spelling would outgrow it, which still holds at least thirteen
  sources of the longest legal identifier. The apply document bound
  (`PINBOARD_DOCUMENT_MAX`) is twice one value.
- **Who may see who notified.** The session remembers at most
  `NOTIFY_SOURCES_MAX` sources seen since it started and answers them through
  the `QueryNotifySources` window request to its own Settings application
  alone (`is_settings_surface`: the attested
  `tairix_taskbar::system::SETTINGS_BUNDLE` under the session's own publisher). The reply is the shared name-list codec the cursor
  sets already used, now one definition for both.
- **The intake is bounded and keyed on the instance.** The notification area
  holds at most `NOTIFICATIONS_MAX` notices in all and
  `SOURCE_NOTIFICATIONS_MAX` per source, a raise past either refused
  `LimitExceeded`, so no program can grow the session without limit. A notice
  is keyed on the attested `ProcId`, never the recyclable pid, so a later
  process under the same pid cannot replace or clear another's, and a reaped
  child's notices are dropped by its pid.
- **The pane** lists the union of the seen sources and the policy's own, in
  identity order, or *None*; states it when the desktop would not say, and when
  the policy is full.

D140 (the loaded notification-icon set is never installed) is the status
glyphs' defect, not the policy's, and stays recorded in
`plans/OPEN-DEFECTS.md`.

### DS11 — Keyboard and Mouse

One double-click interval serves the desktop. Every resolver — the file
manager's listing and chooser, the desktop's icons, the window manager's title
bars — pairs presses under it: `DoubleClickTracker::register` takes the
interval, and `lib/input` keeps no default of its own.

- **Five keys** (`lib/wallpaper::input`): `pointer.primary`,
  `pointer.double_click_ms`, `pointer.speed`, `key.repeat_delay_ms` and
  `key.repeat_rate` (`off` or repeats a second), in two groups
  (`SettingsKey::POINTER`, `SettingsKey::KEYBOARD`) so neither pane posts the
  other's keys. Every span is a `Duration64` in memory; milliseconds are the
  document's spelling alone.
- **The interval is published.** `DesktopInfo` carries it
  (`DOUBLE_CLICK_MIN..=DOUBLE_CLICK_MAX`, decoded fail-closed), which grew the
  desktop notice's payload bound with it. The compositor holds the seat's
  interval for the title bars and `desktop_info` publishes it; the file manager
  reads its own `DesktopInfo`.
- **Applied where the session resolves the event.** `DeviceInputSource`
  applies the button order — a new one waits until no button is held — and the
  speed, carrying the sub-count remainder. `KeyboardInputSource` is the one
  place a key repeats: it drops a device's own repeat of the held key, repeats
  under the policy one per drain, and folds its deadline into the park only
  while a key repeats. A key held across the edge into a lock or another
  session stops. The serve loop reconciles `InputPolicy::of` the settings at
  its head, so every adopt path reaches the sources, and a resumed session's
  rebuilt pointer keeps the policy.
- **The panes.** Mouse offers the three pointer rows, Keyboard the two repeat
  rows and states that there is one built-in layout and no shortcut list. A
  value off a ladder is offered as itself. Every span is set on a slider
  named in words at either end (DS20).

### DS12 — Lock Screen and Screensaver: the idle interface

What it guarantees:

- **One idle deadline** (`idle::IdleClock`): the last seat input, a key's
  repeat included, and `screensaver.after_min` / `lock.after_min` (`never` or
  whole minutes), each folded into the park only while its action is pending.
  A session that cannot verify a password never locks on its own; a resumed
  session starts idle afresh.
- **The screensaver** (`saver::Screensaver`, `screensaver.kind`) is one
  full-screen surface kept over the lock: black, the backdrop dimmed, or a
  slideshow of the shipped catalog, one picture every
  `screensaver.slideshow.interval_s` in its chosen order and category,
  each prepared at screen size through the wallpaper worker's new slide slot
  and the one sandboxed decode — and the animated scenes of DS18. No worker,
  no slides: it stays black rather than decoding on the serve loop. The
  waking gesture is drained into nothing.
- **One lock.** The Lock row, the idle policy and *Lock Now* all go through one
  `lock_screen` routine over `ScreenLock`. *Lock Now* is the `LockScreen`
  window request, honoured for the desktop's own Settings application alone
  and refused `NotSupported` without a broker; a refusal is stated on its row.
  Unlocking always asks for the account's password, which the pane states
  rather than offers.
- **Defaults** are the minimal clock (`ribbon`) after ten minutes and a
  fifteen-minute lock: security is the default.

### DS18 — Screensaver scenes and energy saving

What it guarantees:

- **The pointer goes with the desktop.** A screensaver hides the cursor
  through the compositor's shown/hidden state, kept apart from the cursor's
  artwork, so no refresh re-installs it over the screensaver; the drains
  behind the screensaver and the lock follow the device, so the pointer comes
  back where it is, in the shape of what it is over.
- **Four animated scenes** (`screensaver.kind` = `clock` | `ribbon` |
  `starfield` | `life`), each drawing every other desktop frame, repainting
  only what its frame changed, and moving at most a few frames on a late wake:
  the bar's own clock with the date and *account · machine*, moved each minute
  against burn-in; the minimal clock, its time and spelled-out date held still
  while a ribbon of orange light roams beneath them, each strand one smooth
  curve moving on its own and every one held clear of the text; a perspective
  starfield that cruises, surges into warp and settles back; and four-colour
  Conway's Life on a bit-packed torus, reseeded once it settles. Each honours
  reduced motion from the theme in force — the clock moves at once, the ribbon
  holds still, the starfield only cruises, Life's cells change at once — and a
  scene the heap will not give is a black screen instead.
- **Only the dimmed screensaver builds the backdrop's ground**, so no other
  kind pays for a full-screen surface it discards.
- **Energy saving** (`screensaver.display_off_min`: `never`, or whole minutes
  `0..=1440`, default thirty) counts from the screensaver's start. When it runs
  out the session sends `SetPower` (`plans/DISPLAY.md` D9); a display that is
  off is presented nothing and arms no frame deadline, one that cannot switch
  off keeps the screensaver black and still, and the first input lights it
  before the screensaver goes. The pane's *Energy Saving* group offers *With
  the screensaver*, minutes through a day, and *Never*.

### DS19 — Pictures as settables

What it guarantees:

- **A setting whose choices are pictures is chosen by its picture.** The
  wallpaper and the screensaver are `Chooser`s, each a
  `lib/controls::PictureChoice` seated in its group beneath the rows
  (`plans/GUI-CONTROLS-DESIGN.md` §11.43), in the pane's one scrolling
  column: every picture at the screen's 16:9 shape inside a rounded rim with
  its name beneath. The wallpaper chooser leads with *No picture*, a swatch in
  the backdrop's own colour, then the catalog under its categories; a picture
  in effect the catalog lacks is listed beside its category's, or under the
  directory it sits in.
- **Every screensaver ships a preview** — a 16:9 PNG at
  `/System/Graphics/Screensavers/<kind>.png`, build-discovered from
  `lib/wallpaper/screensavers/` as its own graphics family and refused by the
  image build when a kind lacks one, when it is another shape, or when it is
  smaller than the largest picture a chooser draws. The Starfield, Game of
  Life, Minimal Clock, Ray Tracer, Retro Games and System Monitor previews are
  frames their scenes drew.
- **Pictures are served, and bounded.** A render names its subject — a
  catalog position or a screensaver kind — and its size (`RenderPreview`,
  DS4). The pane asks for the pictures on screen first, then those a screen's
  height either side while memory is plentiful and none beyond. While memory
  is plentiful every picture handed over is kept for the life of the pane — a
  thumbnail of the bounded shipped catalog, so scrolling back asks for nothing
  — and once it is short only those on screen are; a picture rendered at a
  size no longer drawn is let go at any band. Renders go out, each into its own
  region, until the desktop answers that the window has as many pending as it
  has preparers, and resume as each concludes; every one is waited for even
  across a desktop change, whose answer is let go rather than mistaken for
  another's. Each request is a round trip to the session's serve loop, so it
  is carried on a worker of the pane's own, one at a time and each answered
  before the next — the window never waits on it. A rebuild carries the
  pictures it holds across, a refusal is never re-asked, a picture the desktop
  had no memory for is asked again once memory may have been freed (another
  picture fitting, or the pressure band moving), and a landed picture repaints
  its own tile alone. A question about pictures whose answer cannot have changed is
  answered without laying the pane out.
- **The chosen screensaver's own group.** Beneath the chooser, a group named
  for the chosen screensaver holds its options (`SaverOption`: the
  slideshow's interval, order and pictures; the clock's date and who is
  signed in; the minimal clock's date; the starfield's stars and warp; the
  Game of Life's cell size and speed; the ray tracer's processor use; the
  retro games' speed, on the one `Pace` ladder the Game of Life's speed
  shares; whether the system monitor names the busiest tasks), every one kept
  in the document
  whichever screensaver is chosen (`plans/PINBOARD.md`), and ends with
  *Test*. Choosing another screensaver rebuilds the pane in place, keeping
  the keyboard cursor where it was, and lays it out afresh. The search index
  names every screensaver's options, so a search for one reaches the pane
  that sets it.
- **Test is a preview, not a setting.** It hands the session the screensaver
  keys as the pane shows them (`PreviewScreensaver`); the session serves it to
  its own Settings alone, only while nothing holds the seat, reads the
  document strictly as those keys over what it holds, keeps none of it, and
  leaves the preview up through pointer motion in its first moment. A refusal
  is stated on the row that asked.

Host tests: each chooser's listing, selection, sections and fail-closed
adoption; the round's visible-first order, reach, stale-size and refusal
rules; every picture kept across a scroll to the far end and back while memory
is plentiful, and those off screen let go once it is short; carrying pictures
across a rebuild; a landed picture
repainting its tile alone; every chooser picture fitting the preview bound at
every scale; a screensaver choice bringing its own group laid out as a fresh
pane is; every option posting its own key; the slideshow's category row
keeping a category the store lost; Test's document and a refusal's
statement; the session's preview authorisation and strict read, and the
preview's steady first moment; the preview family's discovery and image
checks.

### DS20 — Finding the pointer, and input set in words

What it guarantees:

- **Four pointer aids, one document.** `cursor.shake`, `cursor.trail`,
  `cursor.locate` and `cursor.shadow` join `SettingsKey::APPEARANCE`, set on
  Accessibility's POINTER group (the shadow) and its FINDING THE POINTER group
  (the other three). Shaking is on by default, since it costs nothing until
  the pointer is shaken; the rest are asked for. What each does is
  `docs/src/desktop/cursors.md`.
- **The session owns the aids and they cost nothing at rest.** The shell's
  `PointerAids` is stepped once a frame in `animate` and folded into the park
  only while one is changing. A lone Ctrl is recognised by the keyboard source
  and checked against the pointer source's button record; the wake that takes
  the screensaver down drops it. Under the screensaver nothing is drawn.
- **The compositor draws them as the pointer's own.** The overlay's trail and
  halo sit beneath the cursor, hide with it, and are damaged per part at
  composite time; a ring's damage is its band, not its square.
- **A span is set in words.** Double-click speed and pointer speed run from
  *Slow* to *Fast*, repeat delay from *Long* to *Short*, and repeat rate from
  *Off* to *Fast*: sliders with a stop per ladder step, the value off a ladder
  a stop of its own. A drag posts once, where it settles. Milliseconds are the
  document's spelling alone.
- **The knob is a knob.** `Slider`'s knob is the theme's `slider_knob` wherever
  it is seated, its focus ring stands clear of it inside the control, and
  `with_stops` / `with_ends` give it stops and named ends
  (`plans/GUI-CONTROLS-DESIGN.md` §11.6).

### DS21 — The ray-traced screensaver

What it guarantees (its scenes, sampling and reveal are `plans/RAYTRACE.md`'s):

- **A scene coarse to fine.** `screensaver.kind` = `raytrace` composes a
  scene in one of `lib/raytrace`'s nineteen settings, never the last one's,
  prepares it a bounded unit at a time, then reveals it in
  `lib/raytrace::Reveal`'s order: a first pass of every point of a grid at
  least eight points to the shorter side, each later pass halving the grid's
  spacing, every pixel traced once at the tracer's best and each pass
  scattered over the picture. The picture shown is a cubic B-spline over the
  current pass's grid, a blur coming into focus and exact once whole, and a
  paint repaints and marks only what its steps change. Paints come a scene
  frame apart while the coarse passes form the picture, then further apart
  with the share shown, at most 3 s (`plans/RAYTRACE.md` RT1). It is held a
  minute, faded over three seconds (cut under reduced motion), and replaced.
- **Told as it goes.** A readout window above the picture reads *Generating
  scene... N%* from `Draft::progress` while the scene is prepared, then
  *Rendering... N%* while it is traced, mid-grey in the lower right, and goes
  once the picture is whole.
- **Traced off the serve loop.** `screensaver.raytrace.cpu` sets the share of
  the machine: `idle` (the default) is one tracing thread, `performance` a
  worker beside it for every other core, every one of them kept busy: a scene
  is traced a pass at a time, each core taking the next untraced step as it
  finishes the last and handing what it traced straight on, so none waits on
  another's costly pixel but at a pass's end. The embedder grants the threads
  through the `TraceHost` seam; the loop collects and paints on the reveal's
  cadence through the host-tested `TraceDesk`, which drops whatever was traced
  before the next scene is asked for, holds the thread once what it laid down
  has waited two of the loop's longest waits, and has the thread wake the loop
  through the session's worker wake as each scene is readied.
  The threads leave when the screensaver comes down, the loop waiting on none
  of them. With no thread granted the loop traces a slice a frame itself — on
  its own thread alone under `idle` — and paints on the same cadence.
- **Paced.** A slice is what fits half a desktop frame at the last slice's
  pace, grown at most twofold.
- **Kept when asked.** `screensaver.raytrace.save` (off by default) has the
  engine copy each picture as it traces and the tracing thread write it,
  once whole, as a PNG under `Documents/Pictures/Raytracing/` through the
  `Keeper` seam — named for its setting and when it was finished, never
  written over another, once per scene, and nothing kept with no tracing
  thread granted; a refusal says why on `stderr`.
- **A lost buffer repainted, never retraced.** Once whole, the scene is let
  go. The painter keeps every traced pixel, so a window buffer the compositor
  releases is painted afresh from them; a scene, or a buffer for its picture,
  that the heap refuses rests the screen black a minute before the next is
  tried, rather than being retried every frame.
- Its group on the pane holds its processor use, *Idle time* or
  *Performance*, *Save pictures*, and its *Detail* (DS24), above *Test*.

Tests: the order visiting every pixel once, its first pass tracing its whole
grid, each pass halving the last and ending whole, each pass scattered; the
painter ending on every pixel's own trace however the steps arrive and across
any cores, every paint changing only what it marks, a bounded cover of whole
tiles, a
scattered frame marking little of the screen, a lone point blurring without a
crease, a new point settling in as its neighbours are traced, a pass beginning
where the last ended, a buffer let go painted afresh, stray steps changing
nothing, and a reset starting over from black; the reveal tracing each pixel
exactly as tracing it alone at the best quality does, however slowly;
preparation over slices tracing nothing and telling its progress, tracing
telling its share; the pace, bounded per phase; a slice split across every
worker matching the order traced alone; the next scene set elsewhere; each
whole picture handed over once however long the engine runs on, and an
unheld one reported; the desk's slices reaching the loop in order after what
it has not yet painted, its hold on a loop behind, asking for the next scene
dropping what came before, two asks being one, the signal owed only to a
waiting thread, the loop woken once as each scene is readied, and a real
thread handing its keeper each picture once; the album's folder chain, names,
numbered suffixes, the file decoding to the picture, and its refusals; the
readout's words, place and teardown; the hold, fade and next scene, the cut
under reduced motion, the rest after a refusal or with nowhere to paint, a
lost buffer painted afresh without tracing again, a paint marking only the
tiles about its steps, the paints' wait growing with the share shown from a
scene frame to at most 3 s over a steadily traced reveal, a readied scene
shown from its first steps, steps collected early painted with the next, a
refused scene leaving nothing to the next, the options a crew is launched
with, and on the loop a slice traced each frame and painted on the cadence,
`idle` keeping to its own thread while `performance` uses the pool.

### DS22 — The retro games screensaver

What it guarantees:

- **A flight towards a setting sun.** `screensaver.kind` = `retro_games`
  flies over a glowing grid towards a banded sun between two wireframe ranges
  scattered afresh from each start, the sun's reflection rippling down the
  floor. `screensaver.retro_games.speed` sets the flight on the `Pace` ladder
  the Game of Life's speed shares; under reduced motion it holds still, no
  craft come on, and nothing is drawn after the first frame.
- **Craft now and then, playing retro games.** Two lanes, the sky's and the
  ground's, each rest between acts and seldom play a kind twice running: a
  starfighter's pass or a flying saucer's visit above; a tank battle or the
  riders' duel below. Each act draws from a random stream of its own and is a
  closed course between decisions its own course times, so it plays the same
  however the frames fall; every battle and every duel ends, its survivors
  driving or riding off out of sight.
- **Wireframe in the scene's own style.** Every craft is a convex hull built
  from its corners and verified closed and convex; its faces turned from the
  camera are culled and the rest drawn dark with glowing edges, far to near,
  a multi-part craft ordered across the planes that part its pieces, so a
  nearer thing hides what stands behind it from any angle.
- **A frame costs what moves.** The sky, the sun's glow and the ranges are
  painted once. A frame repaints the floor and the sun's banded part, lays
  back what lay under the craft over the sky from the pixels kept beneath
  them the frame before, and draws the craft from one list of polygons
  replayed a band of rows at a time across the compositor's participants.
  Ground craft stand lower than the camera, so they never rise above the
  horizon. A late wake moves the flight at most `SceneClock::MOST_FRAMES` on.
- **Exact at every distance.** A grid line is the exact area it covers in each
  pixel, through the frame's exposure where it moves; where lines crowd too
  finely to draw they give way to their mean, so the horizon never shimmers.
- **Every edge drawn once.** The ranges are drawn far to near, each edge after
  the nearer of its faces, and the valley floor the two share is drawn by
  neither.

Tests: the scene opaque and whole; a frame repainting the floor and the bands
alone before anything comes on, and the sky only where craft were and are
once they do; into a fresh buffer the whole scene, and in parts what it
paints whole, craft over the sky included; bands run backwards and on real
threads painting what one thread does; the frame cadence, the late-wake cap,
the chosen pace and the reduced-motion hold; every screen shape, and one with
no floor or sky refused; the sun's fit; the exact pulse-train, motion and
sheared-band coverage; the lines' crowding fade; the haze, the reflection and
its ripples; the night, the disc and its sinking bands; the ranges' seed,
valley, horizon and painter's order; every craft's hull sound and no ground
craft as tall as the camera; culling, clipping, the far-to-near order and a
display replayed in bands drawing what it draws whole; the kept pixels laid
back exactly; each lane's timing, its first act either kind alike, and a
seed's show repeated; every pass beginning and ending unseen, facing its
course and banking within bounds; every visit one unbroken run of legs in
sight; every battle ending, playing the same however its frames fall and
wrecking what it strikes; every duel decided with no rider through a fence,
and a bike's body and rider drawn in the order the camera sees them; and the
blasts' spans and resting shards.

### DS23 — The System Monitor screensaver

What it guarantees:

- **The machine at a glance, from across a room.** A header gives the
  machine's name and the time over a verdict pill. Five panels show:
  - the processors — busy share, trend, load, and a cell per core spread
    evenly over its rows;
  - memory — committed share, trend, band, and where it went;
  - the tasks — count, what needs recovery, and the busiest;
  - each storage device — health, throughput and fullness;
  - each interface — traffic and link.

  The board is laid out against a 960 × 540 reference and scaled to fill the
  screen, stacking its panels on a portrait one; a screen too small for the
  smallest scale draws no board.
- **The Switchboard's readings, and no thresholds of its own.** The session
  holds no sysinfo authority. It watches the Switchboard while the board is up
  (`plans/NEW-SWITCHBOARD.md` S14), spawning the monitor once if it is not
  running, and draws the reports that land on its wait set; it never polls.
  The verdict is a fold over the monitor's own pressure latches, the storage
  health and the recovery census, worst first, the rest counted.
- **Never stale as live.** The board waits before the first report and then
  says the monitor is not answering. After `STALE_PERIODS` of the monitor's
  own period without a report, the verdict says when readings stopped and the
  panels dim. A monitor that exits is named as not running.
- **Every line whole, every part named.** A list is laid in as many columns
  as `COLUMN_MIN` seats. A line of detail drops trailing facts whole rather
  than cutting through one, its facts ordered so the least important go
  first. Memory's composition shares the processors' band, so the two traces
  stand level. Its key gives each part as a share of the whole, and a part
  holding anything never reads `0%`. The largest parts are named and the rest
  folded into one, never a lone part, until the key fits.
- **A reading repaints only its part.** Parts are slots that tile the board.
  A report repaints the parts whose readings it changed, and the header
  whenever the verdict moved; nothing is drawn between reports. Each minute
  the board steps round an orbit smaller than its margin, against burn-in.
- **Task names are the option.** `screensaver.system_monitor.tasks` (default
  on) names the busiest tasks; anyone who can see the screen reads them, the
  lock included, which the Settings row states.

Tests: every screen shape tiling its parts on screen without overlap; the
orbit keeping every part on screen; each part drawn inside its own slot
alone, stale or live; a stale panel dimmed; the cells seating every core and
spread evenly; the composition fitting any room, naming the largest, never
folding a lone part, and no part holding anything reading as none; a line of
detail cut between facts; a device's health and an interface's downed link
first; a wide list in columns; the verdict's order and spellings; waiting,
silence, staleness and an exited monitor; a report repainting only what it
changed; the minute's orbit; the serve path refusing an unattested or
malformed report and answering one nobody watches with `BrokenPipe`; and the
watch lease's attest, re-offer and forget.

### DS24 — The ray tracer's scene detail

What it guarantees (the profiles themselves are `plans/RAYTRACE.md` RT39's):

- **A choice of two.** `screensaver.raytrace.detail` is `simple`, the default,
  or `maximum` (`tairix_wallpaper::SceneDetail`); anything else is refused as
  the other options' bad values are, and the settings document parses and
  stages it beside `cpu` and `save`.
- **On the pane.** The ray tracer's group holds a *Detail* dropdown — *Simple*
  or *Maximum realism* — after *Save pictures*, above *Test*.
- **Carried at launch.** The value travels with `cpu` and `save` into the
  session's engine (`saver::raytrace::Detailing`), on the tracing thread or
  the loop alike, with what the machine's memory can spare: its total, read
  once at bring-up, and the pressure gauge.
- **Never refused partway.** Each scene is composed at the detail asked unless
  the band last reported says that detail's `Detail::peak` is not free — a
  band deeper than normal holds at most its exit watermark free, and an
  unreported band holds nothing — when it is composed at Simple instead. Each
  change from the last scene's detail is said once on `stderr`.

Tests: the option's parse, staging and refusals; the pane's row; the options a
crew is launched with; each band's ceiling against Maximum's peak, a machine
smaller than the peak never sparing it; and an engine asked for Maximum
composing at Simple while the band cannot spare it, telling each change once.

### DS13 — the QEMU vertical, and docs

`settings_qemu_aarch64` is a short sibling of the autoload desktop vertical, so
a gate mis-count in one choreography cannot wedge the other. It boots the
autoload root disk, unlocks, logs in, starts `desktop`, opens the capsule's
system menu and chooses *Settings…*, then photographs the window on General,
on Lock Screen's composed form, on Bluetooth's stated absence, and on Storage —
reached past the strip's fold
by the strip's own scrollbar — before paging the strip back up to Appearance,
choosing the appearance the desktop did not boot in,
and photographing the desktop redrawn in it. Its last gesture chooses Compact
density on the same pane.

What the vertical needed, and now guarantees:

- **A witness for a later frame of a served window.** `WINDOW_SHOWN` speaks for
  a first frame only, and Settings holds no `CAP_LOG_EMIT` to announce its own
  panes. So the window is titled with the pane on show and retitles only after
  presenting it, and the session announces `WINDOW_RETITLED` when a frame
  carrying a new title reaches the display: requests are served in order, so
  that frame carries the pane.
- **A witness for the desktop's new look.** `DESKTOP_RESTYLED` follows the
  first frame drawn in a changed appearance, contrast, density, motion or
  scale, after the reveal. It speaks for the session's surfaces only, because
  each application redraws its own window on its own time — which is why every
  window dump is taken before the appearance changes, and the restyled dump reads
  only the bar, the furniture and the wallpaper.
- **The appearance has one route, and it persists.** The Appearance pane is
  the only place the appearance is changed; its choice takes the one
  persist-then-adopt path, and a standing prompt follows any change of look
  through the shell's style generation.
- **PASS is the guest's own four witnesses, in order:** an `APP_LOADED` naming
  the settings bundle, its window's create reply, and two commits of the
  desktop's published document — the pane's appearance choice, then its Compact
  density, a key the first did not change so it commits whether or not
  Settings has adopted the restyled desktop by then — attributed by the path each
  rename replaced.
- **Every press is aimed through the production layout.** The host resolves
  each target from the shell's own geometry (`Shell::strip_row_rect`,
  `strip_scroll_rect`, `setting_rect`, `choice_rect`, and `ScrollBar::part_rect`
  beneath them) and refuses any point the window frame's hit map does not
  answer `Client` for, so no press can land in the invisible resize band.
- **Each dump is read for what its pane draws:** a plate by its top and bottom
  rim, because a plate is filled with the column's own surface; the absence by
  its words on no plate; Storage by a capacity track that is neither empty nor
  full; the restyled desktop by its bar and furniture moving towards the chosen
  appearance over an unchanged wallpaper.
- **Every cut name carries the mark.** Each label, reading, cell, caption and
  title `lib/controls` draws, and the Settings band and statement, are elided
  through the one recipe (`elide_to_width`, then `paint_run`), which the crate
  exports for application-drawn names. The sidebar's plate is `SIDEBAR_WIDTH`
  logical pixels, room in the shipped face and weight for its longest row — a
  disclosed pane's label beside its indent and badge, or the longest category
  label beside its badge — inside the plate's rim with the strip's scrollbar
  carved out; a host test holds every shipped row drawn whole. The app-local cuts elsewhere are `plans/OPEN-DEFECTS.md` D154.

Docs landed with it: the Settings page's General section, the window title,
the vertical, and the corrected Sound statement; the session page's two
witnesses; and the desktop's published document on the confd page.

### DS14 — retire the second form idiom

Every form in the desktop is drawn with `lib/controls::form`, and none carries
a private layout. `datetime.app` composes its six fields from it (DS1), and the
file manager's Permissions tab is an **Access** group (the mode as a reading,
then one Owner/Group/Other row of flags each) over an **Ownership** group.

- **One new slot, `FieldControl::Flags`.** A `FlagSet` is a row of checkboxes
  in one slot, reported as `FieldAction::SetFlag { index, on }`. Every box
  stays whole in a narrow slot and the labels share what is left, and each
  flag leaves room for a denied flag's lock mark after its label.
- **The column placement is shared.** `tairix_controls::stack` (`gap`,
  `plate_width`, `height`, `column_width`, `place`) is the one placement both
  Settings and the Properties window use, and neither keeps a copy of it.
- **The tab has keyboard reach.** The arrows walk rows and flags, Space and
  Enter act, and Tab or Escape give the keyboard back to the tab strip. Paint,
  hit-testing and keys read one placement, so they cannot disagree.
- **A refused session gets the same rows, not a different layout.** Without
  `CAP_FS_CHOWN` the ownership rows carry the Authority Mark and a footnote
  says why; a press or key on them resolves to nothing.

Host tests cover the `FlagSet` contract across the dark, light,
high-contrast and monochrome fixtures, the flags seated whole at the default
size and 200 %, every toggle reachable and apart at the minimum window, the
keyboard reaching all eleven controls, and damage scoped to the rows that
changed. No QEMU vertical dumps the Properties window yet; that is
`plans/NEW-FILEMANAGER.md` FM8d.

### DS17 — the sidebar as one grouped plate

What it guarantees:

- **One plate.** `frame::resolve_frame` seats a `panel` a gap in from the
  window's edges that ends where the pane's column begins, so it stands one
  gap from the plates beside it. The search field keeps the plate's content
  inset; the strip spans its interior and stops short of the rim's rounded
  corners, and its scrollbar is carved from the plate, never from the pane.
  The panel is a group's own plate, painted and rounded through
  `FieldGroup::paint_plate` and `FieldGroup::plate_radius`, so the two cannot
  drift apart.
- **Runs, not headings.** Each `CategoryRow` names its `Group`; a run is
  contiguous in `CATEGORIES`, which a test holds, and
  `CategoryRow::breaks_from` is the one rule both the strip
  (`Tab::with_group_break`) and the shed strip's category `Menu`
  (`MenuItem::with_group_break`) draw by. A break is half the strip's entry
  line.
- **Every row badged.** `PaneRow::icon` is `Some` exactly when its category
  discloses its panes — a single-pane category's row stands for its pane —
  and no two rows share a badge; a test holds both.
- **Lists open independently.** The shell keeps a `DisclosureSet<Category>`,
  closed by default. Choosing a disclosing category opens or closes its list
  and goes nowhere (`StripRow::destination` is `None` for it); going to a pane
  opens its category; a pane on show whose list is closed is stood for by its
  category's row. While a search is in force the query decides what is listed,
  so a category's row goes to its first listed match and a disclosure is a
  no-op. The program library's folders keep the same set, starting open, so no
  list on the desktop carries an accordion policy of its own.
- **The tree keys.** `Tabs` reports `TabsAction::Disclose` for Right on a
  closed entry and Left on an open one, and steps the cursor onto a first page
  or back to its entry; the shell applies a disclosure and keeps the keyboard
  cursor on the category's row while its list comes and goes.
- **A submitted search shows what it matched**: the first *pane* the query
  reached, never a disclosing category's first pane.
- **Theme** states its absence (§3) and names where the appearance and the
  picture are set today.

---

## 6. Sequencing and dependencies

The graph is the ledger's `Depends on` column; this is the reasoning behind the
five edges that are not obvious from it.

**DS1 does not land alone.** It is host-provable by itself, but a shared family
whose only caller is its own gallery tab is one caller, not the two independent
ones `AGENTS.md` §23.3 requires of a shared helper. So it lands with the first
of DS14's two conversions in the same increment — `datetime.app`'s six-field
row is the smaller, and its private arithmetic dies with it — and the family is
proven by a surface that already had a form to draw rather than by a gallery
shaped around it.

**DS2 is independently useful the day it lands**, which is why it comes before
any writing pane: every category is reachable and every absence is honest.

**DS3 is the template, not merely the first pane.** It establishes the
user-scope document and the session's apply policy, so DS4, DS10, DS11 and
DS12 are all further keys in that one document and may land in any order once
it has.

**Shared machinery lands with its consumer.** DS6's argv extension landed in
the same increment as the pane that uses it, so nothing speculative was added
ahead of a caller (`AGENTS.md` §2.4). DS8's `configure` extension is the one
that does not need a pane to have a caller: `configure` is a shipped command
app, so its write side is reachable from a shell the moment it lands, and
DS8b's pane is its *second* caller rather than its first.

**DS8a is the edge DS7 discovered, and it gates two stages rather than one.**
A pane may only show what its own authority can read, and neither the
Ethernet pane's interface addressing nor DS9's other-account fields can be
read by an application holding two capabilities. Both plans answer that the same way —
the administrator-authenticated run shows what it may — and neither can do it
while the broker's reply carries an exit code alone. So the seam is its own
increment ahead of both, rather than being half-built inside whichever of
them lands first. It is not speculative interface: it is added with the first
of its two callers, and DS8b cannot begin without it.

**DS5a is the exception that proves that edge, and is not speculative.** It
moves a derivation that already had a caller rather than adding one for a
caller to come: every item it hoists was live in the Switchboard, `sysmon`,
`fstree`, `stress` or `df` the day it landed, and each of those now reads the
shared definition. DS5 becomes its second consumer. It is a separate
increment only because a half-moved derivation with two callers is worse than
either end state.

## 7. What this explicitly refuses to become

To stay first-class and bloat-free, Settings will **not** grow: a privileged
settings daemon holding the union of every domain capability; a second writer
for any store that already has one; a plug-in or extension surface for
third-party panes (a closed registry is what makes the authority map
auditable); a wizard or "assistant" flow; a scripting or automation surface; a
profile/sync mechanism; a second theming, rendering, or control path; a
duplicate of the Switchboard's monitoring; a second route to a destructive
machine transition; or a control that changes nothing so that a category can
look complete. A domain that belongs to another subsystem is *reached*, never
reimplemented here — and a domain that does not exist yet is *stated*, never
mimed.
