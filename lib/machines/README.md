# tairix-machines

The machine library the emulators share (`plans/BEEB.md` §6): `record` (every
machine's record, written to `library-a` and `library-b` in turn, numbers
minted once, names kept apart, the orphan sweep), `state` (a machine's last
screen and snapshot, each checked on its own, bounded by the application's
largest frame and snapshot), `shelf` (the library window's model), `standing`,
`keeping` (the suspends whose windows stay open until kept), `assistant` (the
new-machine dialog over an application's models) and `menu` (a card's menu,
the icon-bar row and a menu kept up). An application implements
`record::Kept` for its own machine and gives its models and bounds.

The `rt` feature adds the freestanding halves: `store` (the library and each
machine's state read and written over the application's own store) and
`window` (the library window and its dialogs over the window channel, and the
icon-bar presence).

Stability: `experimental`. Consumers: `PCEmulator.app`, `OpenMicro.app`.
Documentation: `docs/src/lib/machines.md`.
