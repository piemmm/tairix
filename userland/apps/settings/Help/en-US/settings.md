## NAME

settings — configure the desktop and this machine

## SYNOPSIS

`settings`

## DESCRIPTION

Opens a desktop window listing every category of setting this system has: what
the machine is, how the desktop looks, the screen, the network, the devices it
is driven with, the accounts that use it, and the volumes it holds. Choosing a
category from the sidebar shows its pane. A category holding several panes opens
and closes their list in place instead, and any number of those lists can be
open at once.

Settings holds no authority of its own. Every change is either a request to the
desktop session, which owns the user's own settings, or a re-authenticated run
of the command that already writes that store, so nothing here can raise a
privilege.

**Appearance** chooses whether the desktop is drawn light or dark, and the
font its text is set in and how large, in points — *Default* is the theme's
own, ten points of Inter, and a size keeps the letters the same size whichever
font draws them. **Accessibility** groups the same contrast, density, motion and interface-scale
settings the way a reader looking for them would, beside the pointer's artwork,
size and shadow; both panes show the shared ones, because a reader looks in
either place. Accessibility also offers three ways to find the pointer: shake
the mouse quickly back and forth and the pointer grows for a moment (on unless
you turn it off), press and release Ctrl on its own and rings close in on the
pointer, or leave a fading trail behind it as it moves. A row takes effect as
soon as it is chosen, so
there is no button to press afterwards; if the desktop refuses a change, the
reason is reported on the standard error stream and the row goes back to what
the desktop actually holds.

**Mouse** and **Keyboard** set the pointer's speed and how quickly a
double-click must follow the first click from *Slow* to *Fast*, and how long a
held key waits before it repeats (*Long* to *Short*) and how fast it then
repeats (*Off* to *Fast*). Drag a slider or step it with the arrow keys; the
change is kept where it comes to rest.

**Trackpad** sets whether a tap clicks — one finger for the primary button, two
for a menu, three for the middle button, and a tap followed at once by a touch
drags — whether two fingers move what is shown, as on a touchscreen, or the
view, as a wheel does, and how far a finger moves the pointer. Two fingers
moving together scroll and two spreading or closing zoom wherever the program
under the pointer zooms.

**Storage** lists every mounted volume: where it is mounted, its filesystem,
its device and medium, how full it is, and whether it is healthy. It is a
report, not a control — mounting and unmounting are the file manager's and
the `mount` command's — and a volume whose format keeps no fixed capacity
says so rather than showing a bar. `df` reports the same figures at a shell.

**Users & Groups** shows your own account, every account's name and user id,
and the machine's groups without asking for anything: a principal may read its
own record and the public name-to-number directories. Everything else — another
account's details, whether an account may log in, and what it is allowed to do
— is not public, so *Show Accounts…* asks for an account that may administer
users and reads the listing under it. That listing is forgotten as soon as you
leave the pane. Editing an account then applies as one command, so a change
spanning two accounts, or a password alongside other fields, is refused with
the reason before any password is typed. A new password is hashed in the window
and never leaves it as a password; setting one still needs an account that may
administer users, and the system refuses anything it must — the pane says so
rather than pretending otherwise.

A category this system cannot serve says so plainly and names what would have
to exist before it could. A control that would change nothing is never shown —
which is why Sound says the audio service offers nothing to set there yet
rather than drawing a volume slider, and why Theme, for which the desktop has
no themes to choose among, points to where the appearance and the picture are
set instead.

The window is titled with the pane it is showing.

Type in the search field above the sidebar to filter it to the categories and
settings a word reaches. `Tab` and `Shift+Tab` move between the search field,
the location trail, the sidebar and the pane; `Up` and `Down` walk the sidebar
and `Enter` opens the row. `Right` and `Left` open and close the list of the
category under the cursor. A window too narrow for the sidebar sheds it, and the
leading crumb of the location trail then lists the categories.

It is launched from the *Settings…* row of the desktop's system menu, from the
desktop's Program Library, or by name from a shell. It requires a running
graphical session: without one the window channel is unreachable and it
reports the refusal on the standard error stream and exits.

## EXIT STATUS

Zero after a clean close; non-zero when the window channel or the shared frame
region was refused (the reason is stated on the standard error stream).
