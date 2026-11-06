# `tairix-unmount` — detach a runtime-attached volume

Stability tier: **experimental**.

The `unmount` command app (`plans/DEVICES.md` D4b): `unmount NAME`
resolves the volume mounted under `NAME` (its catalog name, a directory it is
mounted on, or the file a disk image was mounted from, refused where two
volumes could be meant or the listing keeps only its start; or its durable
root, `id::<volume-id>`, which names that volume and no other) through the
ungated `sysinfo-v1` `MOUNT_LIST` and `VIEW_LIST` queries — whose records
carry each mount's stable volume identity and
availability — and asks the kernel's `volume_detach` path to take it
out of service. A plain detach flushes first and fails closed: an
*unavailable-dirty* volume refuses it, keeping its retained uncommitted
writes for a verified re-insert; an *unavailable-lost* one refuses it so the
loss of the writes it could not keep is acknowledged; a *recovery-conflict*
one still holds its retained set; and a volume a descriptor still holds a
file of refuses it too (`Busy`). Only the refusal a state's writes earn is
told the `--force` consequence; any other is the kernel's errno alone. `unmount --force NAME` is the audited force-unmount: the kernel
revokes the descriptors still open on it, discards the retained set,
retracts the volume, and logs what it gave up — a device volume with its own
event id (4179, `fs.hotplug.force_unmount`), a disk image with its detach's
(4323, `fs.image.detached`, carrying the nodes revoked and whether it was
left unflushed), a network share with its (4303, `fs.network.detached`,
caused `forced`, or `forced_unflushed` when a flush was given up). A path is
matched against the mount points as spelled (`.` dropped, a relative one read
from the working directory), as util-linux's `umount` matches one, and
canonicalised only when it climbs with `..`, passes through a link, or matches
none, so a volume that no longer answers is still named by where it is
mounted; another user's
private volume lists neither its file name nor its bindings, so a miss beside
one says to name it by where it is mounted in `/Storage`. On a healthy volume `--force` still commits cleanly —
nothing is discarded when a clean flush is possible.

A name is resolved by the one resolution `label` shares,
`tairix_procinfo::named_volume`.

The crate is both the pure `no_std` engine library (parser, engine,
seams — host-tested against in-memory fixtures) and the freestanding
`Run` program (`src/run.rs`), which wires the shared
`tairix_procinfo::IpcTransport`, the `volume_detach` syscall wrapper,
and the `tairix_help::BundleHelp` short-help source over the inherited
standard streams. The kernel is the policy point: the user who attached
a disk image or a network share may detach it, the user at the machine's
seat may plainly detach a removable device's volume, every other volume
needs `CAP_FS_MOUNT`, and every decision is audited kernel-side; the tool holds
no ambient authority. A name two users' disk images share (their file
name) resolves to the caller's own, by the owner each mount record names.

`cargo test -p tairix-unmount` drives the parser, the engine (resolve,
force, refusals, the fd-3 `--force` suggestion), and the
help-document switch pinning across every required locale.
