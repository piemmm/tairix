# `tairix-touch`

`lib/touch` is the seat's touch gesture recogniser. A touch driver injects
frames (`tairix_abi::touch::TouchFrame`, see [Input ABI](../abi/input.md)); the
seat owner drains them and feeds them to a `Recogniser`, which answers what
they meant. The desktop session and the greeter both use it, so a tap, a
scroll or a pinch means the same thing at the login screen as on the desktop.

## Surfaces

Each surface is followed apart, keyed by the injecting task the kernel stamped
on the frame and the injector's own device index. A frame names every contact
on the surface; one it stops naming has lifted where it last was. A contact the
device judges a palm takes part in nothing, and stays a palm until it lifts.
Positions are measured in micrometres over the surface's stated physical size;
a touchpad that states none is taken to be a laptop's, and a touchscreen that
states none covers the screen whose size the seat owner gives with
`set_screen_extent`.

At most eight surfaces are followed at once. When another arrives the least
recently fed is let go — its presses released, its pinch cancelled — so an
injector cannot make the seat hold state without bound.

## Touchpad

| Gesture | Meaning |
|---|---|
| One finger moving | The pointer moves by the finger's travel at a gain rising from 4 to 16 pixels a millimetre between 30 and 300 mm/s, scaled by the user's speed |
| A brief tap (≤ 180 ms, ≤ 1.3 mm) of one, two or three fingers | A primary, secondary or middle click, where tap to click is on |
| A touch within 180 ms of a one-finger tap | A drag with the tap's press held; lifting at once instead is a double click |
| Two fingers moving together | A scroll on both axes, 25 scroll units a millimetre, natural or traditional by the user's setting; one that begins along an axis keeps to it |
| Two fingers spreading or closing | A pinch, at the pointer |
| A clickpad pressed | The button its fingers count: one primary, two secondary, three middle |
| A touchpad's own button | That button, mapped through the seat's button order as a mouse's is |

Motion waits while a touch could still be a tap, so a tap clicks where the
pointer was; if the touch turns out not to be one, the motion it held back
arrives whole. While any button is held the finger that moves drives the
pointer — a thumb can hold a clickpad down while a finger drags — and no
gesture begins.

## Touchscreen

One finger puts the pointer where it lands. Its press waits until it moves
past 1.5 mm or rests for 100 ms, so a second finger landing with it begins a
gesture instead of a click; a finger that lifts first is a click at the place
it landed. Two fingers scroll the content with them, ten scroll units a
millimetre, and pinch at their centre, the centre carried with each step so the
application can pan as it zooms. A second finger landing during a press ends
the press; fingers left after a gesture do nothing until every finger lifts.

## Pinch

A pinch begins once the fingers' spread has changed by 2 mm before their
centre travels 1 mm, from a spread of at least 8 mm. Its scale is the spread
relative to the spread when it began, in 16.16 fixed point
(`PINCH_SCALE_ONE`), so an application zooms to the zoom it began at times the
scale and accumulates no rounding. It ends when the fingers stop being two; it
is cancelled — the application returns to where it began — when the seat lets
the surface go.

## Time

Frames carry the kernel's arrival stamp, so a tap is timed by when its frames
arrived rather than when the seat owner read them, and a busy desktop never
reads a tap as a rest. The recogniser states the next instant it must act at
without a frame (`deadline_ns`) — a held tap's release, a touchscreen's waiting
press — and the seat owner wakes for it on its one-shot timer, after feeding
every frame already queued.
