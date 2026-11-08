# tairix-window

Stability tier: **experimental**.

The window-channel protocol engine (`plans/APPWIN.md` AW2): the one
definition of the zero-copy, owner-keyed app-window semantics shared by
both ends of the `WINDOW_ENDPOINT` rendezvous, so the desktop session's
server and every app's client can never drift apart.

- **Server** (`WindowServer`): the engine the desktop session composes.
  It decodes each fixed-width `WindowRequest`, attests the in-flight
  caller through the injected `CallerIdentity` seam (the kernel's
  `call_peer_origin` — an unforgeable `ProcId`, never a claimed id),
  keys every window to that owner, maps the app's endpoint-directed
  `shm_grant` region **once** at `Create` through the shared
  `tairix_display::ShmMapper` seam, and records each `Present`: a window's
  newest frame and the damage since the last one shown, one per window
  however often it presents. `latch`, which the session calls once a desktop
  frame, hands each to the injected `FrameHost` (the compositor's side) as a
  bounds-checked frame slice plus its damage — so a client presenting faster
  than the desktop draws costs one conversion a frame, and the desktop's frame
  work never waits on a present. A present to a window holding no unshown
  frame is answered as it is served; one replacing a frame not yet shown is
  `Served::Withheld` until that frame is shown or refused, or a resize, a
  release or a close supersedes it, so a client is held to the desktop's
  frames. A window holds at most one such answer, every call is answered once,
  and the answers queue in an outbox reserved at window create — a latch never
  allocates — which the session drains with `send_answers`, each reply landing
  in the room its call was posted with, so none waits for kernel memory;
  `release_answers` answers them with no frame while none can reach the
  screen. A resize or a
  release drops what it supersedes, and a frame the host cannot show is
  reported to it once until the window next shows one and is tried again
  with the next frame the desktop composes, never asking for one of its own;
  a later present carries its change as well. A
  `SetBackdropBlur` takes the same owner check and reaches the same
  bridge (`WindowHost::backdrop_blur_set`), so an app frosts the backdrop
  of its own window and of no other. A `Present`, `SetBackdropBlur`, or
  `Close` naming a window the caller does not own is refused `NotFound`
  (no existence oracle); a per-client window cap bounds how much pinned
  memory one app can reserve; a dead client's windows are torn down
  fail-closed via `client_exited`. Routed input is pushed the other way
  with `deliver_event`, which validates the event against the addressed
  window (owner's endpoint, window-local pointer bounds) before handing it
  to the sink. The sink takes the *typed* event, not its wire bytes,
  because only the sink knows whether it goes out now: one that holds an
  event back against a full mailbox folds it by kind and encodes once,
  when it finally goes. A sink that accepts a pick conclusion is answering
  for it either way, which is what clears the pending pick.
- **Client** (`WindowClient` / `WindowEvents`): the app-side half over
  the injected `WindowTransport` seam (`ipc_call` and its posted halves,
  `call_post`/`call_reap`, and `call_await` for one posted request's answer,
  in production). A `present` is posted once the window's previous one is
  answered — the session reads the frame a present names until a later one
  replaces it — and that answer comes with the desktop's next frame when the
  present replaced one not yet shown, so a present waits only for an app that
  outruns the desktop's frames; one that must never wait asks
  `present_in_flight` first and owes its window the paint until the answer
  lands. A
  present refused for room goes as a call, which waits for the room; a
  refused present is handed out like any posted request's refusal, and the
  window's next present declares the whole window. The requests an app makes as
  input moves — `resize`, `set_cursor`, `set_tooltip`, `drag_verdict`,
  `set_title`, `set_sizing` — are **posted, never awaited**
  (`plans/FIX-DESKTOP.md` DESK-21): at most one of each kind per window is in
  flight, a newer one is held as the latest wanted and posted when that one
  is answered, and the endpoint serves a client's requests in order, so a
  posted resize reaches the session before the present painted at its size.
  `WindowEvents::try_wait` takes in landed answers before it drains: a
  refusal is handed out as `EventError::Refused`, which the app reports and
  carries on from, and a refused resize is followed by a `Resized` at the
  size the session kept. A resize asked while one is unanswered is
  `Fitted::Deferred` — no region is made for it — and its size is delivered
  again once that one is answered. `create` validates and sends the window geometry, grant
  handle, event endpoint, title, and the app's `WindowSizing` — whether it
  is resizable and the smallest client extent it can lay out at, `0`
  declaring none — returning the session-minted window id. The minimum is
  a *declaration*: the window manager enforces it (alongside its own
  furniture floor) so a drag stops there, and an app never clamps a
  granted size by resizing its own window back up, which would fight the
  drag once per pointer sample. `present` sends a frame index plus
  damage, never pixels;
  `set_backdrop_blur` asks for the content behind the window to be
  frosted, a radius in logical pixels with `0` off and anything above
  `WINDOW_BACKDROP_BLUR_MAX_PX` refused at decode; `close` tears the
  window down. `WindowEvents` is the app's event stream over the injected
  mailbox seam — never a poll — decoding each delivered `WindowEvent`
  fail-closed. The seam states its two halves as two traits: `EventDrain`
  (`try_next`, which reads a queued frame and never waits) and
  `EventSource: EventDrain` (`park`, the app's own wait-set park).
  `WindowEvents::wait` alternates them, taking in every answer to a posted
  request before each park: the wait-set's reply member stays ready until
  the answer is taken, so parking again first would spin. An app that
  interleaves work of its own drains with `WindowEvents::try_wait`, serves
  input ahead of that work, and parks only when both are exhausted; an app
  whose loop dispatches several wake sources itself — the terminal
  emulator, with a shell stream per window and an animation deadline;
  Switchboard, with a command mailbox and a sampling deadline — keeps its
  own park and takes the drain alone, and still reads through this one
  stream rather than spelling a mailbox drain of its own.
  `EventMailbox` is that drain against the app's own endpoint, dropping
  any frame of the wrong length or from any sender but the session the
  create reply named: the kernel-attested origin is the authentication,
  and it has one definition rather than one per app. The endpoint's id is
  derived from the app's pid, so `bind_event_mailbox` admits only the
  session serving windows to it (`port_admit`), and no other process can
  fill it to starve the session's deliveries. Both paths decode and
  answer a redraw request through one definition, so the polled path
  cannot drift from the parked one. A read that fails says which half
  failed (`EventError::Undecodable` / `EventError::Mailbox`), because the
  answers are opposites — read on past a refused frame, end the channel on
  a dead mailbox — and an `Errno` alone cannot tell them apart
  (`ipc_recv` answers `LengthOutOfRange` for a received length the address
  width cannot hold, which is also a decode refusal). Reading on past a
  dead mailbox would meet the same failure at once and spin.
- **A run of resizes folds to the newest extent.** A client extent is a
  value the window converges on, not an occurrence it must witness, and an
  interactive resize-grab reports one per pointer sample. Every app reads
  through the fold, so an app slower than the pointer lags a frame rather
  than a queue — without it, the samples that piled up during a drag are
  each re-laid-out and re-mapped when the button comes up, and the window
  visibly walks back through the drag before settling.
  `present_damage` decides *what* a round presents from the three cases
  every such app faces (`Repaint::Nothing` / `Reported` / `Whole`), and
  `damage_in` clips a reported client-space rectangle onto the window —
  the app's own fail-closed step, since the session refuses a rectangle
  outside the surface. `pointer_point` widens a wire pointer position into
  the signed geometry the controls hit-test in, saturating rather than
  wrapping, so a coordinate past the range hits nothing. A round that changed the view but reported no
  rectangle presents the whole window: over-covering costs pixels, while
  under-covering would leave a stale frame on screen, because the session
  copies only what a present declares. `present_damage_list` is the same
  three cases for an app that paints each reported rectangle under its own
  clip, and `Owed` is the account such an app folds every round of a drained
  burst into — the strongest conclusion and the rectangles reported, clipped
  to the window and merged by least growth to what one present carries — so
  a burst costs one frame and the account stays bounded. The
  endpoint's *depth* (`EVENT_MAILBOX_CAPACITY`) is defined here, and its
  *name* beside the other pid-derived endpoints
  (`tairix_abi::window_ipc::event_endpoint_for`), once each, because both
  ends depend on them agreeing: the session reads a refused delivery as
  evidence that the owner has stopped draining, which would mean
  different things per app if each chose its own slack.
- **Popup surfaces** (`WindowRequest::CreatePopup`, `PopupSpec`): an app
  opens an undecorated child surface above one of its own windows, so a
  context menu or a settings sheet is never clipped by the window that owns
  it. `WindowClient::create_popup` takes one `PopupSpec` — a `TransientSpec`
  (the parent window, the grant handle, the event endpoint, the frame count
  and geometry) and an offset in physical pixels from the *parent's client
  origin*, since an app is never told its own window's screen position. The
  server validates it exactly as a `Create` and additionally requires that
  the parent is a live window the caller owns (a foreign or unknown parent
  answers `NotFound`, no existence oracle), then hands it to
  `WindowHost::popup_opened` — the session resolves the parent's screen
  position, adds the offset, and clamps the whole popup onto the screen. A
  popup counts against the **same** per-client window cap, so "popup"
  cannot be used to pin more memory than `Create` may. It carries no title
  and no resizable flag: it is never decorated and never listed on the
  taskbar. `present`, `set_backdrop_blur`, and `close` act on a popup's id
  exactly as on a top-level id; closing the **parent** tears down every
  popup keyed to it (as does `client_exited`), while closing the popup's
  own id tears down only the popup. One `TransientSpec` definition serves both
  halves and every transient kind, so the app's request and the engine's
  validated view cannot drift. A transient hangs only from a top-level window:
  a popup, a tool window, a dialog or a layer surface named as a parent is
  refused `NotSupported`.
- **Tool windows** (`WindowRequest::CreateTool`, `ToolSpec`): a floating
  palette hung from one of the app's own windows, which the window manager
  frames with a mini title band reading its title and the user moves. It is
  validated, owned, budgeted and torn down exactly as a popup, through the
  same engine path (`WindowHost::tool_opened`); `ToolSpec::carry` asks for the
  press the parent still holds to carry it on, and its moves come back as
  `WindowEvent::ToolMoved`. `WindowClient::create_tool` and
  `WindowPane::open_tool` open one.
- **Dialog windows** (`WindowRequest::CreateDialog`, `DialogSpec`): a form, a
  question or an operation's progress, drawn by the app in a window of its
  own that the session frames, centres over its owner, fits to the screen and
  names by the app's attested identity (`WindowHost::dialog_opened`). It is
  validated, owned, budgeted and torn down as a popup is; its
  `DialogModality` says whether it holds its owner as an alert does, and it
  takes the keyboard only where the app has earned the front, spending the
  activation that earned it. An alert may hold a dialog, and closing a window
  dismisses every alert holding it or a transient that closed with it.
  `WindowClient::create_dialog` and `WindowPane::open_dialog` open one.
- **The seat's desktop is asked for here, and kept current here.** An app
  cannot draw honestly without knowing the screen it is on, the desktop's
  UI scale, and whether the theme runs light or dark — and the compositor
  that owns all three is another process it must not reach into.
  `WindowClient::desktop` asks for them as one `tairix_abi::desktop::
  DesktopInfo`, before the first window is created so the opening frame is
  already the right size at the right density in the right colours; the
  server answers from the injected `WindowHost::desktop`, which reads the
  live compositor rather than a cached copy. The query is read-only and
  carries no capability: it describes the caller's own seat, names no
  other principal's data, and grants no authority. `Desktop` is the
  app-side holder — it resolves the reported percentage into a `Scale`
  (refusing, never clamping, one outside the range `Scale` admits and
  keeping the last good value), reports the screen as a `Rect`, caps a
  wanted window size to it with `fit_window`, and `apply` adopts a
  `WindowEvent::DesktopChanged` (which the session pushes to every live
  window, `WindowServer::window_ids`) and answers whether anything
  changed. One definition of that bookkeeping, so no app repeats it.
- **The redraw handshake is answered here, not in every app.** The
  session may drop its copy of a window's pixels and then send
  `WindowEvent::RedrawRequested`. `WindowClient` remembers each window's
  last presented frame index and current extent, so `WindowEvents::wait`
  re-presents that frame with full-window damage before handing the event
  on — behind a present still in flight rather than waiting for it — one
  definition of the answer instead of one per app. The event is still
  delivered, so an app that would rather render genuinely fresh pixels can;
  a window that has never presented has nothing to re-send and the event is
  a no-op. A window whose frames the session *released*
  (`WindowEvent::ContentReleased`) has nothing left to re-send, so its
  redraw is the application's own whole-window paint: `is_hidden` answers
  whether the session has yet asked for it again, and until it has, nothing
  is presented, since re-attaching would map again the memory the release
  gave back.

- **The app-side shell** (`app`, bare-metal targets only): the bring-up
  sequence every windowed `Run` binary repeated verbatim. `RtWindowTransport`
  is the production transport — `ipc_call` for a call, `call_post` and
  `call_reap` for a posted request. `bind_event_mailbox` binds the process's
  one event mailbox and builds the wait-set with the machine's
  memory-pressure band, the desktop notice and the window-reply wake already
  on it, reserving `EVENT_TOKEN`, `PRESSURE_TOKEN`, `DESKTOP_TOKEN` and
  `REPLY_TOKEN` so an app numbering its own members from `FIRST_APP_TOKEN`
  cannot collide (a reply wakes the park as `Wake::Event`, since the event
  drain takes it in); `park` / `park_until` wait on that set
  and answer a typed `Wake`, so the *decision* about a band change has one
  definition while the *response* (which caches to hand back) stays each
  app's own. `bring_up_desktop` asks for the seat's desktop and primes a
  `ThemeRegistry` from its appearance. `mode_for` / `region_bytes` /
  `BYTES_PER_PIXEL` shape the window mode and its frame region once, so a
  create, a resize, and every present agree; `region_bytes` is checked rather
  than saturating, because a wrapped length asks for a region too small for
  the window it describes and a saturated one for a region no machine can map
  — and on a 32-bit target the product of two `u32` dimensions genuinely does
  not fit. `ShellError` carries the
  reserved exit code (81–84), the reason, and the typed `Errno` — leaving only
  the application's own name for it to prefix. The errno is carried rather than
  derived from the code, because one code covers several distinct refusals: a
  window already being open and a surface that could not be allocated are both
  `EXIT_NO_WINDOW`, so a service whose own contract answers in `Errno` (the
  Switchboard host) would otherwise have to report an out-of-memory as a
  programming mistake.

  `WindowPane` is **one window**, however many the app has: the id the
  session knows it by, the shared frame region, and the layout both are shaped
  as. `open` and `open_popup` are the create dance — size the region, create
  it, grant it, ask the session — with every refusal unmapping what it had;
  `open_popup` additionally refuses a create reply that did not come from the
  session that opened the parent, closing the window it named rather than
  drawing into it. The region holds `FRAME_COUNT` frames, two: `present`
  draws into the one the session is not reading, first bringing it up to date
  from the caller's surface with what the other frame was presented with since
  (`tairix_geometry::StaleFrames`, the record the display client keeps for its
  own frames), converts the named rectangles through the one window-frame
  codec, and presents exactly those, drawing only once the window's previous
  present is answered (`FrameRing`), since the session reads that frame until
  then. It re-attaches a released region and sends it whole, and presents
  nothing while the window is hidden or while the re-attach waits for room.
  `resize`'s ordering is the load-bearing part: the
  spare region is created and granted *first* and adopted only once the
  session has accepted the resize, so a refusal drops the spare and leaves the
  old geometry standing and drawable — it answers `false`, which means "still
  at the old size", never "broken". `resize_with` is the same for a window
  whose picture is a plain `Surface`: the fresh surface is allocated before the
  session is asked and swapped in only once it accepts, the one resize every
  such app — single- or multi-window — makes. `close` answers what the session
  said and consumes the pane either way, so the region is unmapped even on a
  refusal.

  The pane deliberately holds **no picture**. What a window looks like is the
  application's: a plain `Surface` for most, a screen model carrying its own
  cell diff for the terminal emulator. A pane that owned a surface would force
  a second window-sized allocation on every app whose retained picture is not
  literally one.

  `AppWindow` is the **single-window** pairing: the channel, one pane, and the
  surface every frame is drawn into. `present` takes the paint as a closure —
  the shell owns the frame-region and damage bookkeeping without owning a pixel
  of anyone's window — and promotes the damage to the whole window when the
  session has released its copy, since a released region holds none of the
  pixels a partial present would leave standing (`content_released` exposes the
  same fact to a caller that must resolve a reported damage set before
  presenting). `try_present` is the same for a paint that can refuse: a refused
  paint presents nothing and the window keeps its last frame. Either way, a
  rectangle a refused paint or a failed present left torn is re-sent with the
  next present, whose paint is clipped to cover it. `retained_damage` is that
  decision, host-tested: every rectangle is clipped to the window before it is
  painted or recorded, so one named past the surface is sent as the part inside
  it and can never widen later presents into rectangles the frame codec
  refuses. Its `resize` is the pane's `resize_with`, so a window the app could
  not draw into is never left on screen.

  A **multi-window** app (the terminal emulator, the file manager, the viewer)
  holds its own `WindowPane` per window — and per popup — beside whatever
  retained picture it actually paints from, and takes the shell's free
  functions for the rest. `watch_wake` puts a `tairix_rt::work` worker's
  answer wake on the app's wait-set (nothing to watch for a worker that never
  started), `declare_app_bar` declares an icon-bar presence, answering an
  `AppBarRefused` whose words the app states and carries on, and `report` /
  `fail` state a reason on `stderr` under the app's name — so no app spells
  any of them itself.

- **A window's document file** (`document`): where a document came from and
  is saved to, the save in flight and the saves asked for behind it, and a
  file chooser open for it — the sequencing apart from the syscalls, so every
  order is a host test. One save is in flight at a time; plain saves asked in a
  row become one of the latest document, every Save As is its own, and closing
  writes every chained save at once. A save that lands renames the document
  after the file a Save As made, makes it writable and says so, through the
  `SavedDocument` the application's engine implements.
- **A window's refusals and warnings** (`notices`): `Notices` states them one
  alert at a time, in the order they arose, each source keeping at most one
  waiting, so the queue is bounded by a window's sources rather than by how
  often one speaks. `refuse` raises an error alert (what was not done, and
  why), `warn` a warning alert (what was done short of the whole). On the
  bare-metal targets `app::refuse` states a refusal on `stderr` and in an
  error alert, holding no window when the refusal is that one could not be
  opened. All build the alert with one `notice_alert`, the reason stated as a
  sentence.
- **An application's own menu** (`menu`): `MenuBuilder` builds the bounded
  `AppMenu` a row at a time, onto the root plate or a submenu, leaving out a
  row the menu cannot hold rather than the whole menu.
- **The document host** (`docapp`): a window per document in one resident
  process. The contract the engine is driven through — `DocumentView`, the
  `Outcome` an input comes to, the `Request`s every document window makes
  alike — is host-tested with the engines. Each engine lays itself out for
  its window's size and keeps what it was laid out as (`DocumentView::lay_out`,
  as an `AppView` does), so no call hands it a layout of another's. On the
  bare-metal targets `run`
  is the whole program of an application that edits what the user handed it
  (TextEdit, Paint, Draw): opening, closing and quitting with changes asked about
  first, the trusted picker — and through it a file to import, handed over
  read-only to `DocumentApp::import_chosen` (`Request::Import`), as a file
  another application drops where the view answers it places it is, with the
  point (`DocumentView::places`, `OpenTarget::Placed`) — documents handed over
  on the icon bar, painting
  only what changed, and the one queue saves are written on in the order
  asked. Text is drawn without waiting on the font service: what the font
  client has not yet fetched is drawn provisionally, a thread of the host's
  own fetches it, and every window is laid out again when it lands. A save
  beyond a window's share of that queue grows it by a room for
  as long as the save is outstanding, a closing window keeps a room for each
  save it leaves behind, and the process ends only once every save has landed.
  What differs per application is `DocumentApp`: reading a document in, its
  own requests and workers, its pixels, the icon-bar rows of its own after
  *New window*, and the windows of its own that hold no document — a settings
  window, say — each an `AppView` the host opens, routes, paints and closes
  (`NoAppView` for an application with none), closed by Quit only once the
  quit can no longer be turned down, and one asked on a document window's
  behalf closed with that window or as it moves on to another document
  (`Host::open_app_window`'s owner), a control of one that changes on its own
  stepped on the deadline it names (`AppView::deadline_ns`, `advance`). A save
  is checked before any file is touched (`DocumentView::refuse_save`), told
  whether it writes the document's own file or another (`SaveTarget`), since
  only the document's own file records a format its name may not say; the
  media type of a file another application offers is read once per drag for
  every window it is offered to.
  `docapp::settings` (feature `settings`) is the settings window itself: the
  shared `SettingsFrame` over an application's closed registry implementing
  `docapp::settings::Preferences`, each event one `SettingsRequest` — the
  edit, from what to what and whether it settled, then *Restore defaults* or
  an ask of the application's own. `DocumentApp::leaving` gives the
  application its turn to see its own workers out before the process ends.
  A document window's view may tear palettes out into tool windows
  (`DocumentView::tool_window`): each shows a rectangle of the window's own
  drawing laid out beside it (`ToolWindow::fits_beside`), and after every
  round the host opens, resizes, retitles and closes them to match, routes
  their input to the view in its drawing's coordinates, hands it their moves
  and close marks, and paints what the view reports inside each into that
  window alone. The view hears it has the keyboard while the window or any of
  its tool windows does.
- **A tooltip is asked for once per tool** (`DeclaredTip`): the region and
  line a window last asked a tip for, so pointer samples over one tool cost no
  further request, a line that changes over the same region is asked for
  again, a line the wire cannot carry withdraws the last tip rather than
  leaving it standing, and a session that refuses tips is not asked again
  until the tip wanted changes.

The wire format itself lives in `tairix_abi::window_ipc`; this crate adds
the behaviour. Both halves are host-proven in `src/tests.rs` against an
in-process loopback (a real `WindowServer` behind the client seams), so
the request semantics have exactly one tested definition. The production
wiring — the session serving the reserved endpoint from its waitset loop
and the app parking on its event endpoint — lands with the session and
app bundles (`plans/APPWIN.md` AW3/AW4).
