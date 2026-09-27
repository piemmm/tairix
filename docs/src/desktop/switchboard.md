# Switchboard monitor service

`userland/gui/switchboard` (`tairix-switchboard`) is the **Switchboard
monitor service** (`plans/NEW-TASKBAR.md` T10–T12): a small, dedicated
process the desktop session spawns as the logged-in user, which samples the
live system through the System Information API, feeds the taskbar's
always-right-most account capsule its tray signals, and hosts the live
overview window that capsule opens.

## Role

The tray overview needs system-wide authority the desktop session's own
manifest should never carry. Isolating the sampling in its own
capability-sized process keeps that authority out of the session
(`AGENTS.md` §5.2): the session merely receives compact summaries over IPC
and hands them to the taskbar's tray model.

Each 2-second cycle samples the process list (stopped-process count and the
top CPU consumer since the previous sample, keyed on the stable
`proc_id`), the aggregate CPU busy fraction, and — every fifth cycle, to
bound the audited query's rate — the kernel memory-pressure band. A pure
derivation turns the sample into the wire `TraySummary`: CPU pressure with
enter/exit hysteresis (≥ 900‰ / < 800‰), the dominant of the CPU/memory
pressures with a pressured-resource count, and a validated top-task name.
Every field is a real measurement or an honest absence — a failed or
refused query degrades exactly the field it backs, never fabricates one.

## Channel

Summaries travel over the seat-scoped `SWITCHBOARD_ENDPOINT`
(`lib/abi/src/switchboard_ipc.rs`), which the **session** binds and this
service calls as a client. Publication is change-only against the last
acknowledged summary, with a 10-second keepalive that doubles as orphan
detection. A successful publish replies with the serving session's
`ProcId`, which is how this service learns the one identity it will accept
commands from.

Commands travel the other way, over the per-instance mailbox
`command_endpoint_for(<own pid>)` that this service binds: `OpenPanel` (show
the overview on a named section), `SeatReport` (which window owners the
session's liveness vigil finds unresponsive), `Power` (the machine
transition the user confirmed in the taskbar's quick-actions menu — see
[Power transitions](#power-transitions)), `FrameReport` (what the
session's last composited frame cost — see
[The Resources section](#the-resources-section)), and `OwnerBundle` (which
application bundle one window owner was launched from, so a task row can draw
that application's own icon). Every command is authenticated
against the kernel-attested sender of that very message, never a claim on
the wire; a command from anyone but the attested session, a command that
arrives before any session has been attested, and a frame that does not
decode are each dropped with a stated reason and never touch the model.

## The live overview window

`OpenPanel` shows this application's own `Switchboard` screen composition
(`src/view/`, one module per section around a shared skeleton) on the
requested section, through `Switchboard::select_section`. An ordinary tap on
the taskbar's tray capsule asks for **Resources**, so the window opens on the
processor — what the machine is doing, which is what a reader arriving at a
system monitor came for; a long press still asks for `Recovery`, which is an
explicit destination. The screen is
assembled entirely from the shared `lib/controls` controls and paints no
chrome of its own; it lives in the application rather than in `lib/controls`
because it arranges those controls into one particular window
(`plans/NEW-SWITCHBOARD.md` S1).

The window is cut from the icon bar's glass (`WINDOW_GROUND`,
`SurfaceGround::Frosted`): its bare ground lets the blurred desktop through at
the bar's weight — `chrome_alpha` over `chrome_backdrop_blur` — while everything
laid on it stays solid: the rail's entries, the Tasks table's rows, every block,
card, tile and button.
The service asks the compositor for `Theme::backdrop_blur` as the window opens,
before its first frame, and again on every desktop change. The frame and title
bar the window manager draws stay opaque.

The window manager decorates the window server-side (the frame, title bar,
window commands, and resize grabber — see `plans/COMPOSITOR-WORK.md`);
Switchboard draws only its client content, beginning with the **navigation
rail** down the leading edge: one vertical `Tabs` strip listing every subject
the surface can show — the task list under `TASKS`, each resource device under
its own group heading, and the recovery list under `RECOVERY` — each entry
carrying its own reading and a bounded `Chart` of it. The rail is the whole
switcher: pressing an entry, or walking the cursor onto it with Up/Down once
the rail holds focus, shows that subject's pane. On a rail the cursor *is* the
choice, so browsing shows what it names rather than waiting for a second key.

The rail is never shed. It is the only route between subjects, so a drop order
that could take it away would strand the reader wherever they happened to be;
`MIN_WIN_WIDTH` therefore includes it, and a section's own frame is resolved
in what is left. Both the pointer and the keyboard run the one transition
`Switchboard::select_section` runs.

A rail taller than its column **scrolls rather than truncating**. Its own
scrollbar is carved from the rail column's trailing edge only while the rail
overflows, so the pane beside it never narrows. The wheel scrolls the rail
while the pointer is over its column, and the section's list everywhere else.
Whichever route changes the subject — a press, the keyboard cursor, or the
host opening the window on Recovery — the rail scrolls that subject's entry,
with the heading that introduces it, into view.

There is at most **one** window: a second
`OpenPanel` asks the session to raise the existing one (naming this
service's own pid) and switches section rather than stacking a second. The
window's close control destroys it and the service returns to headless
sampling — sampling and publishing continue unchanged whether or not a
window is open, because the window is a view onto a monitor that never stops
monitoring. The system is re-sampled strictly on its 2 s deadline: an input
or command wake never re-queries the system.

The model is rebuilt on the same sample cadence, and the panel presents at
most once per wake — and only what it still owes the screen. That account is
authoritative: a wake that reports no rectangle presents nothing at all, so a
pointer move that crosses no control, and a sample whose readings all read the
same, each cost no render and no present.

A present that does happen covers what the wake reported and no more. The
window holds one surface for its whole life, so the render is clipped to that
rectangle and only those pixels are copied into the shared frame: every pixel
outside it is the one already on screen. Every control the input path reaches
reports the rectangle it redraws into one sink the panel owns, so hovering a
row costs the row it left and the row it entered; a composition-wide transition
reports what it re-lays instead (a scroll marks its list and its bar, and a
subject change the whole client, since the pane beside the rail is replaced
outright). A keyboard focus move reports every ring and Focus Field it moved,
including the ones a control's plain setter cannot report itself.

**A fresh reading reports the instruments and cells that moved**, not the
client. The reading is adopted against the very frame the composition will next
be drawn in, so each section compares what it derived against what it held:
Tasks reports the visible rows whose cells moved, Recovery the fault cards the
sample changed, and Resources
the pane items whose readings moved, its pressure banner when the banner's
words moved, and its device rail and command column when either did. A banner
that came or went has moved the pane beneath it, which is then reported whole.
A list that gained or lost an entry has moved everything below the change and
reports its list whole; the rail is the host's to report,
because it is shared chrome rather than any section's region — and because it
states every subject's reading, a reading from a subject that is *not* on show
still costs that one column, never the client. Measured over the fixture window, a sample that moves every task's CPU
cell costs 185 µs against 916 µs for the whole client, and presents 442×144
pixels instead of 760×560 — which is also what the session's serve thread pays
to decode the frame.

The two sections that are *not* on show adopt the same reading and report
nothing: they draw no pixel, so a rectangle resolved against a frame that is
not theirs would name another region's.

**Selecting reports what the selection re-derives, not what was pressed.** A
rail entry names the pane beside it and a fault card names the detail beside
it, so both routes to a selection — the press and the cursor the keyboard
moves onto it — report the pane or detail, the commands that describe it, and
the marks the strip or the cards moved. Reporting the pressed control alone
would leave the reader looking at the previous device's readings until
something else marked the window whole. The scrollbar is the round's too where
the selection left the section holding a different number of items: the
controls it routed through know nothing about a bar that is not theirs, and the
bar is re-ranged by the next paint, which is too late to report but exactly in
time to be drawn inside the reported rectangle.

A change no round could describe — a resize onto a new surface, a desktop
appearance or density change, or a session that discarded the window's retained
pixels — marks the whole window. A round that moved something and reported
nothing leaves it stale, which is why reporting is each section's stated
obligation and why a section over-reports where the two pull against each
other. What the window carries:

**Three sections, one per question a reader arrives with:** what is running,
what is this machine doing, what broke.

| Section | Source |
|---|---|
| Tasks | the sampled process list, as a sortable table whose every row opens its own task's menu — see below |
| Resources | one pane per resource *device* the sample names: the processor, the machine's memory, each storage device, each managed interface, the display path, and the machine's own identity, seats and authority — see below |
| Recovery | stopped processes this service sampled itself, plus the seat report's unresponsive owner ids **joined against those same sampled names** — the report carries ids only, so an owner this service never saw produces no row rather than a fabricated one |

A resource the service could not measure this cycle reads `unknown` with an
unmeasured meter, never a fabricated `0%`.

### One anatomy, one drop order

Every section lays out into the same frame — an optional sidebar, header and
footer around a primary column with an optional detail pane, impact column and
action rail beside it — resolved in one place, so no section improvises its own
geometry (`plans/NEW-SWITCHBOARD.md` S3).

A window too narrow to seat everything **sheds** the optional columns in a
fixed order — detail, then impact, then rail, then sidebar — rather than
squeezing the primary column, which is shed below no floor but the one pixel
under which it would not exist. The window's own minimum client width is a
readability floor, not the width at which every optional column happens to
fit, because shedding one is the drop order working as designed.

A section whose primary column is a list of `Card`s — Recovery — is a
master/detail screen, and **pressing a card selects it**: a completed click
anywhere on a card's own body, clear of any footer button it carries, makes
that card's subject the selected one, so its detail pane, impact column and
action rail all describe the card the reader just pressed. Where a card
carries footer commands, a click on one selects the card *and* resolves that
command, so a command can never act on a subject other than the card that
offered it. A card that is not actionable — disabled, or denied by authority —
selects nothing. The walk over the cards the viewport shows is the one the task
rows use too, rather than one written per section, so a second list cannot
drift into a different idea of what a press means, and the keyboard cursor
selects the card it lands on for the same reason.

### Scrolling

Every list scrolls **a pixel at a time**, as a desktop scroll view does: the
task rows, the fault cards, a resource pane's flow and the rail are each laid
out at their natural size and shown through a viewport that can rest at any
pixel. A row, card or chart the reader has scrolled part-way past is drawn
whole and cut by the viewport's edge — the processor's chart keeps its full
height and simply slides — never squeezed into what is left. A wheel detent
moves a list the shared wheel step, already accelerated by the seat, and a
fraction of a detent is carried rather than dropped; an end button or an
arrow key on a focused bar moves one row or card, and a page keeps its last
line in view. Bands pinned above a list stay put — the Tasks column headings
and a resource's pressure banner — and a pointer over one reaches no hidden
part of the line scrolled beneath it. Walking the keyboard cursor onto a line
scrolls the least that shows it whole.

What is lit follows the pointer, not the content. A wheel turn, a keyboard
reveal or a fresh sample that clamps a list moves its lines under a pointer
that did not move, so the round replays the resting pointer: the line now
under it lights, the line carried away — even clean out of view — goes out,
and those two are all the replay reports beside what the scroll already did.

The commands stay put while the list beside them moves, so the Resources
command rail lights an **Edge Wake** down its leading edge exactly while the
pane is scrolled away from its start, and puts it out on the way back.
Recovery's rail, beside fault cards, lights none, and the Tasks table has no
rail beside it. Only the turn that lights or puts out the wake repaints the
rail; every turn after it repaints the list and its bar alone.

### The Tasks table

Tasks is the rows and nothing else (`plans/NEW-SWITCHBOARD.md` S4): the column
headings are pinned inside the table, and what may be done to a task is its
row's own menu. Sorting is an arrangement of the rows already sampled and issues
no query, and the table always shows the latest sample.

**One row per program.** A parser sandbox worker is its owner re-entered as a
capability-empty child, so it carries its owner's name and account. The
sampler folds each process the kernel marks sandboxed into the row of the
owner its parent link names: the worker's CPU, memory and disk are added to
the owner's, and the task count, the tray's top task, the stopped count and
the top consumers are all taken from the folded rows, so every surface agrees.
A worker whose owner is not in the sample keeps a row of its own.

The **rows** are a sortable `TableHeader` over nine columns: Task (its icon and
name), Owner, State, Activity, CPU, Memory, Disk, Network, Core. A row's icon
asks for the launching *application's own* picture first: the desktop session
reports which bundle it launched each window owner from
(`SwitchboardCommand::OwnerBundle`), because the kernel's process record
carries a name and no image path. Every other process resolves its picture from
that kernel-attested name through the fixed program-store order, and a name
that resolves to no bundle draws the executable class glyph.

**Every application draws its own icon, decoded away from this service's
authority.** The manifest requests `CAP_FS_ACCESS` to read the launching
bundle's declared asset under this service's own attested identity, and
`CAP_SANDBOX_SPAWN` — not `CAP_PROC_SPAWN` — to decode those untrusted bytes.
The narrow capability admits exactly one shape of child: a kernel-branded,
capability-empty parser worker this binary re-enters itself as. So the monitor
gains no general authority to start a process, and a malformed PNG is never
decoded beside the system-wide process scope, task control and power authority
this process holds. Real reach stays per-inode: it reads only what the
launching user could read, and an account whose ceiling withholds
`CAP_FS_ACCESS` simply gets the glyphs.

The read and the sandbox round trip both happen on a worker thread, never on
the loop that owes the window a frame. A paint *records* what it missed and
draws the built-in glyph for that frame; the worker's wake — a permanent
member of the same wait-set the loop already parks in — brings the pixels and
the next frame shows them. One wake per drained batch, so a table of fifty
rows costs one repaint rather than fifty. A kernel that refuses the thread or
the wake pipe reports it once and every row keeps its glyph; the read never
moves onto the loop. Each answer is retained once per (kind or asset, pixel
side) in the panel's artwork cache and blitted thereafter, and the cache gives
memory back on the memory-pressure band wake.

Every column is a *reading* about the task. The sort is the header's own and
stable — rows a column cannot separate keep the order the sample reported them
in. *Activity* is the task's own CPU sparkline, drawn into that column's rect;
the column geometry has one definition, which the heading, the cells and the
sparkline all read. A working task draws no line under its row: the trend
belongs in the column whose heading promises it.

**A task's commands are its row's menu.** A secondary press on a row selects it
and asks the desktop for that task's menu at the press; Enter or Space on the
row the keyboard is on does the same, hanging the menu from the row once it is
scrolled into view. The menu is the desktop's chain like every other
([menus](./menus.md)), titled with the task's name: Switch to and Reveal
window, then Pause, Resume and Lower priority, then Open logs, then Force quit
last with the destructive emphasis. A command the task cannot take is listed
disabled with its reason, shown as a tip on dwell, and the reason is the true
one: the task's own state is asked first — exited, already paused, not paused,
already at the background level — and the caller's authority only after it
("needs process-control authority", for want of `CAP_PROC_CONTROL`). A menu
row cannot carry the Authority Mark, which only the desktop may draw, so the
reason is what tells the two refusals apart. *Open logs* is always disabled: no
capability-gated query for a task's own log entries exists.

**The answer acts on the task, never on a row.** The menu names its task by
the never-reused `proc_id`, and samples keep landing while it is up, so a
chosen row is resolved against the model held when the answer arrives: a task
that has gone is acted on not at all, and a command it no longer permits is not
carried out. The panel holds the one open the desktop owes it an answer for and
drops an answer naming any other.

The content cursor spans the column headings, then the rows, so the headings
stay reachable whatever the sample leaves showing — including nothing — and a
sample leaves the cursor, and the heading it rests on, where the reader put it.

The census tiles, the filter strip, the search field, the command rail and the
footer band the concept boards sketch are **retired**: the tiles' readings are
the Resources section's subject, the strip's kinds needed a job registry and a
service manager that do not exist, and a task's commands belong to its row.

#### What the table measures, and what it cannot

*Owner*, *State*, *CPU*, *Memory*, *Core* and *Activity* are read per sample. *Disk* is a
real rate: the service deltas each task's read-plus-written byte counters
against that task's *own* previous reading over the interval between the two
samples. A cumulative total is not a rate, so the first sample, a task seen for
the first time, and an interval nobody measured each yield no reading; a
counter that did not move over a real interval is a genuine `0`. *Activity*
plots a bounded per-task ring of the CPU shares already measured, keyed by the
never-reused `proc_id` so a recycled pid cannot inherit a dead task's history
and an exited task leaks neither its history nor its counters.

*Network* has no interface at all — there is no per-process socket accounting —
so it renders the explicit unmeasured mark. An absent reading is never a `0`,
never a dash that reads like one, and never a plausible number.

### The Resources section

Resources is **one pane per resource device**, instrument-led. A vertical
`Tabs` **sidebar** — the device rail — lists what discovery actually found,
grouped: `Resources` (the processor, the machine's memory), `Storage` (one
entry per storage device), `Network` (one per managed interface), `Graphics`
(the display path), then `Machine` (identity and uptime, sessions and seats,
permissions and limits). Each entry carries its name, its current reading and
its own bounded trace, so the rail is a live summary of the whole machine and
the pane is the detail of one part of it. The `Machine` entries carry no
trace: they are facts rather than rates, and the absent instrument is what
says so.

**A `Storage` entry is a device, never a mount.** `VOLUME_IO_STATS` reports
the *device's* cumulative counters — every volume on one disk reads the same
fold, and the record names the serving block endpoint beside the volume id
precisely because of it — while the boot namespace projects one writable
volume at `/` and at each flag-bearing subtree beneath it. So the section
groups the mount table before it draws anything: one entry per serving
endpoint, carrying the volumes on it and the paths each is reachable at. Per
mount, one disk's throughput would be reported once per projection *and* once
per partition, and its counters would be deltaed against themselves — a
second fold of one sample sees the first fold's own reading as the interval's
earlier end, derives nought, and plots a flat trace for the disk the machine
is running from. Where the kernel publishes no serving device for a volume
there are no shared counters to collapse, so that volume stands as its own
entry with its capacity alone; a mount with no backing volume at all (the
in-RAM layout directories) is view plumbing and no storage device.

**A `Storage` entry is named by its device, then by what is on it.** The
entry reads `<device> · <volumes>` — `virtio-blk · ARXFSSystem · ARXFSRoot`
for the one QEMU disk the boot floor brings up — because a reader scanning
the rail is choosing which *disk* to look at, and naming the entry after the
volumes on it names a filesystem instead. The device's name comes from the
device: its driver declares it (`Block::device_name`) and it rides the same
ungated `VOLUME_IO_STATS` record the grouping key comes from, so a session
that may read no queue depth and no health can still name what it lists. The
volumes follow so two disks of the same kind are still told apart by what is
on them, and a device whose driver declares no name is named by its volumes
alone rather than by an invented identity — which the pane's capacity block
states as an absent reading, never as a fabricated one.

**How full a device is, is the share of the whole medium.** A storage
pane's capacity block reads `used / total`, where used is
the capacity less what is *unallocated* — so a format that withholds a
metadata reserve is not reported as having spent it. The block states the
reserve plainly by naming its rows for the figures they carry: `Capacity` is
used of total, and `Available` is what an ordinary allocation may still
consume, which on a reserved format is the smaller number. This is
deliberately not `df`'s `Use%`, which divides by what a caller may allocate
rather than by the medium and so reads higher on a reserved format; both
derive from the one shared `lib/procinfo` model, which names each so neither
surface can pick the wrong one (see [`sysinfo`](../abi/sysinfo.md)).

**Every rail entry with a rate behind it carries a trace, from the counters
the service deltas itself.** A storage device's figure is how busy it is — the
share of the interval it had a request outstanding, its `busy_ns` delta, the
same utilisation its service block states — so the entry reads like the
processor's, and how full it is stays the capacity block's to say. Its trace
is the throughput its byte counters delta into. An interface's figure is the
rates query's already-averaged reading, which states its own averaging window
beside the figure so nothing inherits it, while its trace is that interface's
cumulative counters over *this* service's sample interval, through the same
fold a storage device's uses. Memory's trace is its committed share's own
bounded history, recorded beside the CPU's through one series definition, so a
refused reading on either side never shortens the other. Only the `Machine`
entries have no instrument.

**A byte rate is drawn against the scale its own window needs.** A rate has
no ceiling, and a fixed one either flattens ordinary traffic or clips a fast
device: plotted against a gigabyte a second, the half a mebibyte a second a
desktop disk moves drew under a pixel. So each device's history is held as
bytes per second and drawn against the least power of two at or above its
peak across both directions — at least 64 KiB/s, so one metadata write on an
idle device cannot fill the box. The busiest point in view reaches at least
half the box, the scale comes back down once a burst scrolls out of the
window, and the pane's axis row states it (`512.0 KiB/s full scale · read
above, write below`), since a height with no scale is not a reading. Across
devices, the figures beside the traces are what compare.

**A trace is tinted by what it means, and a two-directional one by which way
the bytes went.** Most entries read as their own resource, so the trace wears
that resource's rail hue. The two subjects that are not devices carry their
own signals instead — the task census is *what the machine is running*, which
is not compute saturation, and the recovery entry is recovery — because a task
count drawn in the compute hue read as a second CPU trace beside the real one.
A storage device and an interface are *duplex*: reads rise above the axis in
the read hue and writes mirror below in the write hue, receive against send
likewise in the network pair, so a read-heavy and a write-heavy device never
look alike. That colouring has exactly one definition, which the rail entry
and the pane's own hero both draw through, so the sidebar and the pane it
opens can never tint the same reading differently — and the rail therefore
shows a storage device's writes, which plotting only its reads did not.

**The rail's length is discovered, never declared.** Twelve cores, four disks
and three interfaces is the design case; a hundred-core machine with a dozen
disks gets a scrolling rail, not a truncated one, and no entry count is a
compile-time constant.

**A group with no entries states why, in its own rail position.** A heading
is drawn by the entry that *starts* its group, so an empty group would
otherwise vanish and leave a reader unable to tell a machine with no such
device from a session that was refused the inventory. `Storage` and `Network`
are the two groups that can be empty, and the report carries the verdict the
sample reached for each: the rail draws the heading with one line under it —
the refusal where there was one, "No storage device is present." where the
query answered and found none. The `Tabs` sidebar draws these through
`with_absences`, which selects nothing, takes no keyboard cursor and shifts
no entry's index, so a statement drawn among the entries can never move the
device a press lands on.

Cores are deliberately **not** rail entries: the CPU pane shows every core at
once, so a per-core rail would state the same readings twice and push the
devices off screen.

Each pane is a **hero** — the device's headline reading, its context lines and
its instrument — over **blocks** of the detail behind it. The instrument
belongs to the reading, not the renderer: a rate trends, because its shape
over time *is* the reading and it has no fixed ceiling to fill a bar against;
a fraction of a measured whole tracks. A fact pane has neither, and that is
what says its readings are facts.

A block holds whatever its reading *is* — a composition, a grid of per-core
cells each with its own trace, the tasks costing the device most, a status
pill the health buckets resolve to, or genuine facts. Rendering a resource as
key/value text is the defect this section exists to fix.

**One block anatomy.** A block — the hero, a pane's detail block, a per-core
cell, a fault's fact and timeline blocks, and the action column Resources and
Recovery each carry — is a
hairline-rimmed plate a step lighter than the section behind it, under a
small-caps accent title with a hairline rule. It is composition over the
shared plate primitives rather than a control, because `Panel` is a different
anatomy (a header band at control height, a dominant rail, a signal bead, an
actions row) and is shared with the terminal, the taskbar and the file
manager, so retuning its caption to match would retune those three. The paint
and the layout read one definition of where a block's content lands, so a
command is hit-tested and focused exactly where it was drawn.

Two regions deliberately wear no plate. The **device rail** is a list of
destinations rather than a block of readings: its selected entry lifts and
marks its leading edge, and its group headings carry the accent. The
**Recovery detail pane** *is* the detail region, and the fault's identity line
at the top of it is the heading that says what it describes — it used to be a
titled `Panel` whose caption was the fault's name *and* draw that name again
inside itself, so the name appeared twice.

A block whose body brings its own plates draws none of its own, and its title
draws no rule. The per-core grid is the case: each cell draws the same plate,
which is what tells one core's figures from its neighbour's, and a plate
around the grid would nest one rim inside another. The tile inside a cell is
*unplated*, so a core's name, trace and two readings share one surface; its
performance class is an outlined, toned `StatusPill` — orange for a throughput
core, green for an efficiency one — because a resting pill's wash reads as
nothing at badge size.

**The hero's figure leads and its unit trails quietly.** The figure is set in
`TextRole::Display` — the one figure a surface is built around — against a
body-size unit on the same baseline, and it therefore carries no unit of its
own: a spelled-out percentage would render `18% % busy`.

**The per-core grid spreads evenly and every cell is one size.** How many
cells a row can seat is a function of the pane's width, so the grid wraps
rather than squeezing; when it wraps it *balances* — four cores in a pane
three cells wide draw as two rows of two, not a full row and a lone
straggler. Every row then divides the grid's own column count, so a row that
cannot be filled leaves its trailing slots empty instead of stretching its
cells across them: a reader compares core against core by eye, which a cell
three times its neighbour's width defeats. The column count is a layout input
to the compile, so a resize recompiles the flow and the scroll range keeps
describing what is on screen.

**A resource under pressure wears a banner on its own pane**, above the hero:
the band, how long it has stood there, and the relief the model recommends. A
cause and its resource were never two places. The banner is pinned across the
top of the pane and the pane's readings scroll beneath it, so the pressure and
its relief stay in view however far the reader scrolls. A band's age has no
interface behind it — nothing timestamps a band change — so the service clocks
it off the monotonic uptime reading and reads unmeasured where there is none,
never a fabricated zero.

**A storage device's service readings are two-sample deltas, never a served
average.** `VOLUME_IO_STATS` publishes the device's cumulative bytes,
completed requests, busy time and summed waits, and `VOLUME_IO_QUEUE` its
occupancy and the `BlkDeviceClass` budget bounding it; the pane derives
throughput, IOPS, utilisation, await, service time and mean queue depth over
its *own* sample interval, so no consumer inherits another's averaging window.
A first sample, an unmeasurable interval, and an interval in which nothing
completed each state their absence rather than reading as an idle disk. The
two queries carry different gates on purpose — a utilisation figure is one
every user may see, a queue depth is a driver internal — so a session without
`CAP_SYSINFO_KERNEL` still reads its throughput and await while the two queue
rows say which refusal they met.

**The CPU, Memory and storage panes each carry the five tasks costing that
resource most**, from the per-task readings the process record already
provides, so a pane and the Tasks table can never disagree. **Summing them is
not the device's total** and the block says so: filesystem, RAID and swap
traffic belongs to no process. The interface pane has no such block — per-task
network has no interface at all — and states that in words rather than showing
an empty list, because an empty list reads as *none*.

**The Graphics pane is named for the display path, not for a GPU.** A
framebuffer-only or headless machine has no GPU and would read an empty *GPU*
pane — but it still composites, and that work is what a reader needs. So the
pane leads with the compositor's measured frame cost and treats the device as
one of its facts. The reading that earns the block is damaged pixels against
blended pixels against screen pixels; every figure is a count of work, and no
duration rides this path, because a duration is neither reproducible nor
assertable. A frame that recomposed nothing reads *idle* rather than a row of
zeros pretending a frame was drawn, and a frame nobody has reported yet reads
unavailable — only the session that owns the compositor can count one.

**The rail entry states the damage, and its trace plots the damage per
frame.** The rail's figure is what changed on screen; the hero's is the layer
contributions blended to resolve it, two magnitudes apart, so the two are
different readings rather than one stated twice. The trace's full scale is the
frame's own screen: the only reference a per-frame pixel count has, and the
one the hero's context line already spells the reading against, so a
full-screen repaint fills the box and a cursor-sized frame sits near the
bottom because that is what it cost. A byte reference would be the wrong
dimension, and the sample that carries no report contributes no point rather
than a nought that would read as an idle frame.

**The device's own readings come from the display service that drives it**
(`GPU_DEVICE_STATS`, gated with the hardware tree it details). Its
compositor's layer limits and per-layer opacity fill the compositing-path
block; its scan-out mode, its interval utilisation, and the memory it owns
fill the device block. Utilisation is a delta over the sample's own interval,
never the service lifetime's average, so a first sample states no share; a
device with no memory of its own says so in words, because that is a different
statement from none being free. A **per-engine** breakdown still has no
producer — no display driver reports its engines separately — so that row
carries the honest unmeasured mark rather than one device's occupancy dressed
as an engine's.

**A device's commands are labelled, not glyphed**, and almost none has an
endpoint. Of the commands the panes offer, only "sort tasks by *resource*" is
one this service can carry out: it is a view transition onto the Tasks table,
ordered by that device's own cost, so a busy device is traced to the tasks
sitting on it. Every other command is drawn *plainly disabled* for want of an
endpoint rather than marked for authority — acquiring a capability would not
make an absent endpoint appear.

**Selecting a device performs no I/O.** The rail's selection changes which
pane is drawn from state the sampler has already delivered: it issues no
query, opens no store and waits on nothing, and a pane with no sample yet
reads unavailable rather than blocking for one.

**When the window is too narrow to seat the rail, its *route* moves into the
band** as a `ComboBox` naming the current device, whose list is the same
device set the rail held. Losing the rail must not lose a pane, so what
replaces it is a control rather than an omission.

### The Recovery screen

Recovery is the one screen about a *single* fault at a time. The **primary**
column is one `Card` per fault — what faulted, what happened to it, and how
long ago — and the three columns beside it all describe whichever card is
selected.

Selection is remembered by the faulting task's own kernel-attested identity,
never by its row number. The list is rebuilt from scratch every sample, so a
number would silently re-point at a different fault the moment one above it
cleared; the identity survives a reorder and drops only when the fault
genuinely goes. That rule has one definition, which every section with a
selection to keep reads.

The **detail** pane names the fault and the task it is, a `StatusPill` naming
what the fault costs while it stands, a `FactList` of its status, its age and
the recommendation on the shared block plate, and then a `Tabs` strip over
three pages, whose selected page draws on a plate of its own:

| Page | What it reads |
|---|---|
| Timeline | the marks this service observed: the fault itself, stamped with the age it has stood, and — where that age is known — that it is still standing |
| Crash Snapshot | the kernel's own crash record for that task: the fault class and the distance from its anchor, the access direction, the owning uid/gid, `pc`/`sp`/`fp`, every named register and every backtrace frame |
| Logs | no log-query interface exists, so the page states that |

The crash record is matched to its fault by process identity and nothing
else: a numeric pid is reused, so matching on one could attribute a dead
task's crash to a live task that inherited its number. A fault with no record
says so plainly — a task the kernel stopped, or one merely gone
unresponsive, has faulted without ever raising a user fault, so that is a
statement of fact and deliberately does not wear the unmeasured mark.

The **impact** column is titled and stacks four unplated `MetricTile`s for the
faulting task's own CPU, memory, disk and network — titled but unplated,
because the tiles are the readings the resource panes already draw and a
plate around a stack of them would nest one inside the pane's own. Network is
always unmeasured: no query reports a process's network use, so the tile says
so rather than showing a zero.

A fault's **age** is tracked by the service, not read from the kernel: there
is no state-change timestamp anywhere in the System Information API, so the
service keeps when it first saw each task faulted, keyed by that same stable
identity and clocked off the monotonic uptime reading. With no uptime reading
there is no clock, and the age reads unmeasured rather than as a fabricated
zero. An entry is dropped the first sample its task is no longer faulted, so
a task that recovers and faults again is timed from its *new* fault.

The **rail** is `RECOVERY ACTIONS` for the selected fault, carrying only the
commands this service actually backs: Restart, and Force with its
confirmation posture (or the Authority Mark when the caller may not take it).
The **footer** states how many faults have cleared. That count is observed
history — only something that folds one sample into the next can see a fault
disappear — so it is counted where the samples meet and carried in the model,
which is what keeps a refreshed screen and a freshly built one the same
screen.

The content cursor walks the fault cards, then the page strip, then the
rail's commands. Moving onto a card selects it, so the detail, impact and
rail always describe the card the reader is on. A rail stop hands the key to
the button, so a refused command refuses the keyboard exactly as it refuses
the pointer.

### What is deliberately empty, and why

- **Background jobs** — there is no job registry anywhere in the OS to
  enumerate, so no section shows one. It returns as a `Jobs` tab and a
  `Type` column on Tasks the day a registry lands, not as a section.
- **Services** — the System Information API (`lib/abi/src/sysinfo.rs`) has
  no service-enumeration query, so nothing claims to list them.
- **A graphics device's engine utilisation and video memory** — the hardware
  tree names the device, but no query publishes its per-engine busy time or
  its memory, so those facts are marked. The pane leads with the compositor's
  measured frame cost precisely so a machine with no GPU still reads usefully.
- **Per-task network bytes** — no per-process socket accounting exists
  anywhere in the system; attribution belongs to the network service, which
  owns the sockets. The interface pane states that in words.
- **Temperature, AC and battery** — no sensor or power-supply interface
  exists, and no driver to serve one.
- **In-panel system actions** — the machine's power transitions are not
  rows *in this window*: they live in the taskbar's quick-actions menu,
  where the user confirms them, and reach this service as the `Power`
  command below. Session lock is the desktop session's own surface (it
  keeps the session running behind it), never this service's.
- **An accelerator pane** — there is no accelerator device class for
  discovery to report, so the rail grows no `Accelerators` group and the pane
  does not exist. It arrives with the driver class, not as a greyed-out
  teaser.

Offering a control that would fail at the point of use is worse than an
honest absence, so these stay empty.

### Actions

| Control | Effect |
|---|---|
| A Tasks row's menu | `WindowRequest::OpenMenu` for that task, at the press or the row; its one `MenuClosed` answer acts as below |
| Task *Switch to* / *Reveal window* | `SwitchboardRequest::ActivateOwner { owner }` to the session — raising the window is how this system shows a reader where it is, so both commands make the same request |
| Task *Pause* / *Resume* | `signal(pid, Stop)` / `signal(pid, Continue)` on the menu's task — requires `CAP_PROC_CONTROL` |
| Task *Lower priority* | `sched_set_priority(pid, Low)` on the menu's task — requires `CAP_PROC_CONTROL`, and is spent on a task already at `Low` |
| Task *Force quit* | `signal(pid, Kill)` on the menu's task — requires `CAP_PROC_CONTROL` |
| Task *Open logs* | nothing: no journal-read query exists, which is why the command is disabled |
| Resource *Sort tasks by …* | resolved inside the widget: shows the Tasks table ordered by what that device costs, so a busy device is traced to the tasks on it |
| Every other resource command | nothing: no endpoint exists to drive a reclaim, a scrub, a trim, an unmount, a lease renewal or a clipboard, which is why each is drawn plainly disabled |
| Recovery *Restart* | `SwitchboardRequest::RestartOwner { owner }` to the session |
| Recovery *Force* | `signal(pid, Kill)` — requires `CAP_PROC_CONTROL` |
| Window *Close* | destroy the window, return to headless sampling |
| `Power` command (from the session) | `system_power(action)` — requires `CAP_SYSTEM_POWER`; see below |

Every row's availability reflects what this service can *genuinely* do: it
queries its own effective capability set through `cap_query`, compares each
row's kernel-attested owner uid against its own, and a control whose
authority is absent renders with the Authority Mark — a task's menu row states
the refusal as its reason instead — and is never attempted: the verdict is
re-checked at apply time against the model then held, so what is offered and
what is enforced cannot disagree. A sampled task id that does not fit
the `signal`/`sched_set_priority` signed width is refused rather than
truncated into a different, arbitrary process. A refusal from the kernel or
the session is stated on `stderr` and leaves the model untouched; it never
ends the service.

### Power transitions

Restart and Shut Down are drawn by the taskbar, confirmed by the user in the
session's modal dialog, and **performed here**. The desktop session
deliberately holds no power authority: it is the largest, most exposed
process on the seat, so the widest-blast-radius capability in the system
stays out of it. It relays the confirmed choice as one `Power` command on
this service's authenticated mailbox, and this service — already seat-scoped,
already authenticating that mailbox, already stating its refusals — performs
the capability-gated `system_power` syscall under its own identity.

The check happens twice on purpose. This service refuses without
`CAP_SYSTEM_POWER` before asking the kernel anything, and the kernel checks
the caller again on the far side of the trap. A granted transition never
returns; a refusal (an absent capability, a platform with no primitive for
the transition) is stated on `stderr` naming the transition that did not
happen, and the machine keeps running.

Whether the capability is genuinely held is **published, never assumed**:
every tray summary carries a `power_capable` flag re-read from this
service's own effective capability set at that moment, so an authority the
user's ceiling withholds — or one dropped since start-up — stops being
advertised on the very next publish. The session passes the flag through to
the taskbar, which renders the two rows with the Authority Mark and emits
nothing while it is false. An absent, dead, or not-yet-published service
leaves them denied: fail closed, never optimistic.

## Waiting

The loop is tickless and event-driven: **one** `waitset_wait` per iteration
covers the termination signal, the command mailbox, the machine's
memory-pressure band, and — only while a
window is open — that window's event mailbox, with a timeout equal to the
time until the next real sample is due. Sampling is strict: a cycle
triggered by an input or command wake before the deadline is a no-op that
never re-queries the system. There is no poll loop and no
sleep. The window's event source joins the set when the window opens and
leaves it when the window closes, so a closed window's channel is never left
armed. Its folding event stream (`tairix_window::WindowEvents` over an
`EventMailbox` keyed to the identity the create reply attested) is created
and dropped with the window for the same reason; the mailbox itself is the
process's and outlives any one window, so an event still queued from a
window that has gone is dropped on the id it names rather than applied to
its successor. The periodic re-sample is the documented polling fallback: the
system-wide metrics expose no change event to park on.

## Capability sizing

The manifest requests exactly `CAP_CONSOLE_WRITE`, `CAP_SYSINFO_GLOBAL`,
`CAP_SYSINFO_KERNEL`, `CAP_SYSINFO_HW` (the hardware inventory — the
per-interface network facts the Network page and the network tile are built
from, and the seat list the Session page reads), `CAP_SHM` (the zero-copy
window frame region the session maps, as for any windowed app),
`CAP_PROC_CONTROL` (delivering a control signal to a task this service did
not spawn — the Force action), `CAP_SYSTEM_POWER` (the machine transition the
session relays here rather than performing itself), `CAP_FS_ACCESS` and
`CAP_SANDBOX_SPAWN` (a launching bundle's icon, read and then decoded in a
capability-empty worker — see [The Tasks table](#the-tasks-table)), and
`CAP_LOG_EMIT` (its own log records); the kernel intersects them with the
launching user's ceiling at spawn, so an ordinary account's instance simply
publishes that it is not power-capable and the desktop's power rows stay
refused. The three optional sampling
scopes are probed **once** at startup (capability sets are fixed at spawn;
re-probing would only spam the audit log with denied audited queries):

- an administrator's instance sees the system-wide process list, the
  memory-pressure gauge, and the hardware inventory;
- an ordinary user's instance degrades cleanly to self-scope — its own
  processes, the ungated overall CPU fraction, no memory signal, and no
  interface or seat inventory.

A refused scope is an answer, not an error; the service publishes what it
can honestly see.

## Lifecycle

Started by the desktop session after login; never by PID 1. Exits — each
with its reason stated on `stderr` first:

- a termination signal → clean exit;
- `NotFound` / `PermissionDenied` from the endpoint (no session, or the
  session refused a stale instance) → clean exit — the service has no
  purpose without a session to report to;
- five consecutive **faulty** publish attempts, or a wait-set failure → a
  stated abnormal exit rather than an unbounded silent retry or a busy loop.

`WouldBlock` is explicitly **not** one of those faults. A call endpoint at
capacity refuses the post outright rather than blocking, so a full queue
says only that the session has not drained it yet — the transient
back-pressure condition the kernel defines it to be. It is not evidence of
a fault here nor of an absent session, so it costs the service nothing: the
summary stays unacknowledged, the change gate re-offers it on the next
sample, and one attempt per sample period is paced by the sampler rather
than a retry loop. Counting it as a failure meant a desktop that was merely
busy for five sample periods killed the monitor watching it, and nothing
restarts one — the tray capsule then stayed dead until the user pressed it
again after the session had reaped the corpse.

Design details and constants live in the crate's rustdoc and
`userland/gui/switchboard/README.md`.
