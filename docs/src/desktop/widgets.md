# Widget gallery (`widgets.app`)

The widget gallery is a first-class demo desktop app that showcases every
shared Reactive Alloy control (`lib/controls`, `plans/GUI-CONTROLS-DESIGN.md`)
on its own tab, each with several role/state/value variations. It is the
worked reference for how an application composes the shared controls: every
widget it shows is drawn and driven by `lib/controls`, and the app adds no
second control implementation.

## Layout

The window's furniture (frame, title bar, command buttons) is drawn
server-side by the compositor, so the app presents only client content: a tab
strip selecting one control family, and a panel of captioned demo widgets for
the selected family. A panel taller than the window scrolls beneath the strip
through the shared `ScrollView`, with a `ScrollBar` beside it holding its one
offset; each tab opens at its panel's top. The families are:

| Tab | Controls |
|---|---|
| Buttons | `Button` (primary / recommended / destructive / disabled / denied), `IconButton`, `SplitButton` |
| Selectors | `Toggle`, `Checkbox` (checked / mixed), `Radio` (a single-selection group) |
| Values | `Slider` (plain / stops named *Slow* to *Fast* / capped / disabled), `Progress` (fraction / busy / failed) |
| Text | `TextField` (editable / placeholder / read-only / invalid), `SearchField`, `TextArea` (wrapped, with and without a placeholder) |
| Choice | `ComboBox`, `Menu` |
| Collections | `ListRow`, `TableRow`, a vertical `Tabs` sidebar list — two badged sections that open and close their badged pages independently, by pointer or the tree keys, and a plain one set apart by a group break — `Card`, `Panel` |
| Forms | `FieldGroup` — a captioned plate of `FieldRow`s holding a toggle, a combo box, a slider, a text field, a refused setting, a reading, a stated absence, and a command; and a group holding a `PictureChoice` of swatches and pictures under section titles |
| Bars | `Toolbar`, `ScrollBar` (vertical and horizontal) |
| Feedback | `Dialog`, `Tooltip`, `HelpTip` |
| Window | the `WindowControl` command buttons (close, minimize, put-to-back, size toggle) |

## Interaction

Click a tab, or use `Left`/`Right`, `Home`/`End`, and `Enter` on the tab
strip, to switch panels. Click a widget to interact with it (a toggle flips, a
slider moves, a combo box opens); a clicked widget keeps the keyboard focus, so
arrow keys, `Enter`, `Space`, and typed characters then drive it. `Tab` and
`Shift+Tab` move focus between the tab strip, the panel's interactive widgets
and, while the panel scrolls, its bar, which the arrow, page, `Home` and `End`
keys then move; focus landing on a widget scrolls the panel the least that
shows it. Each control emits its typed action, which the gallery — the control's
owner — reflects straight back into the control; nothing here performs
privileged work.

Pointer events go to the widget under the pointer, and a move away tells the
widget it left, so every widget shows its hover whatever holds the keyboard
focus. A press is held by the widget it began on until its release, so a drag
that leaves it still reaches it; an open choice list holds the pointer and the
keyboard until it closes, so a click outside it only closes it and `Tab` does
not walk away from it. The wheel scrolls the widget under the pointer: a scroll
bar or a text area by the desktop's one wheel distance a detent, a toolbar too
narrow for its tools by one tool. A turn the widget under the pointer does not
use scrolls the panel instead, and an open choice list is drawn over the strip
and the bar rather than cut at the panel's edge.

## Presenting what changed

The gallery is the worked example of an app that presents the rectangle it
repainted instead of its window. Three whole-window passes used to run on every
pointer sample: a window-sized surface allocated and zeroed, the gallery drawn
into all of it, and every pixel unpremultiplied into the shared frame under a
full-window damage rectangle — after which the session diffed the whole window
again.

Now the `Run` binary holds one surface for the life of the window, and each
round of input carries a damage region (`tairix_controls::damage::sink()`) that
the controls and the gallery report into. `tairix_window::present_damage` turns
that into the rectangle to present: what was reported, clipped to the window;
the whole window for a first frame or an adopted desktop change, which re-themes
and re-densifies every pixel; and nothing at all when nothing changed. The draw
is clipped to that same rectangle, which is sound precisely because the surface
is retained — every pixel outside it is the one already on screen.

The gallery asks for a present whenever a round reported anything, so a hover
that changes no value is still shown. A round that changed the view but
reported nothing presents the whole window. Over-covering costs pixels;
under-covering would leave a stale frame, since the session copies only what a
present declares. That safety net is not a substitute for reporting: host tests
render the gallery before and after every event of scripted walks over all ten
panels — hovering, pressing and releasing every widget, actuating the whole
focus ring from the keyboard, and turning the wheel over every widget — and
assert that every pixel which changed lies inside what that round reported.

## Structure

The app is `userland/apps/widgets`. Everything with behaviour worth testing
lives in the crate's host-tested gallery-model `[lib]` (`tairix_widgets`): the
`GalleryTab` families, the per-family panels of `DemoItem`s, the `DemoWidget`
enum that gives every control a uniform render/pointer/key/focus surface, and
the `Gallery` that lays a panel out and routes input. The freestanding `Run`
binary is a thin shell that composes the gallery over the window channel
(`lib/window`), exactly as the file manager composes `lib/browse`.

It requires a running graphical session; without one the window channel is
unreachable and the app reports the refusal on the standard error stream and
exits. It needs only `CAP_CONSOLE_WRITE` (fail-loud diagnostics) and `CAP_SHM`
(the window frame region).

## Tooltips are the seat's, not a control's

`Tooltip` draws the plate, and that is *all* it does: a control never shows
its own tip. An application declares one — `WindowRequest::SetTooltip`, a
window-local `WindowRegion` and one short `TooltipText` — and the desktop
session owns everything else, because everything else belongs to the seat: the
dwell before it appears, where the plate goes so it stays on screen, and every
reason it comes down again.

That split is not a convenience. An application is never told where its window
sits on screen and never learns a pointer position inside the seat, so it
could not place a plate truthfully or time a dwell even if it owned them. A
window holds at most one declaration, so a second replaces the first, and
empty text withdraws it — one operation, with no second "hide" to fall out of
step with. The plate is drawn in an **input-transparent** compositor window,
so the tip that appears under the pointer cannot take the hover it exists to
explain.

Two things declare tips. An application declares them for its own client
pixels, as above. The desktop declares them for **its own menu rows**: a row
that cannot be chosen states why on dwell rather than in a caption beside its
label, which is what used to make a plate as wide as its longest excuse
([menus](./menus.md)). The two cannot collide — a declaration is keyed on
either the compositor window presenting an application or the menu chain
itself, never on a bare id both could mint.

See `plans/TOOLTIPS.md` and [the window manager](./wm.md).

## Container pointer routing

A container (`Toolbar`, `ActionRail`, `Panel`, `Dialog`, and a `Card`'s
footer) hit-tests one pointer sample **once** and delivers it to at most
three children: the one the pointer left, the one it entered, and any child
holding a press. Every other child is already at rest and would only be
written back the state it has, so a motion sample over a crowded strip costs
one rect test rather than one per child.

The pressed child stays in the stream wherever the pointer travels — the
pointer grab. Its own latch resolves against the position it last saw, so
dropping it would leave that position stale and a press dragged off the child
would fire on release instead of cancelling.

Routing is an optimisation, not a behaviour change: the state a scripted
pointer path leaves behind is identical to feeding every child every event,
and `lib/controls` pins that with a differential test against the fan-to-all
delivery it replaced.
