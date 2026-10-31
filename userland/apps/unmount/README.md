# `tairix-unmount` — detach a runtime-attached volume

Stability tier: **experimental**.

The `unmount` command app (`plans/DEVICES.md` D4b): `unmount NAME`
resolves the volume mounted under `NAME` (its catalog name, a directory it is
mounted on, or the file a disk image was mounted from, refused where two
volumes could be meant or the listing keeps only its start) through the
ungated `sysinfo-v1` `MOUNT_LIST` and `VIEW_LIST` queries — whose records
carry each mount's stable volume identity and
availability — and asks the kernel's `volume_detach` path to take it
out of service. A plain detach flushes first and fails closed; a
surprise-removed (*unavailable-dirty*/*unavailable-lost*) volume
refuses it, keeping its retained uncommitted writes for verified
re-insert, and so does a volume a descriptor still holds a file of
(`Busy`). `unmount --force NAME` is the audited force-unmount: the kernel
revokes the descriptors still open on it, discards the retained set,
retracts the volume, and logs what it gave up — a device volume with its own
event id (4179, `fs.hotplug.force_unmount`), a disk image with its detach's
(4323, `fs.image.detached`, carrying the nodes revoked and whether it was
left unflushed), a network share with its (4303, `fs.network.detached`,
caused `forced`, or `forced_unflushed` when a flush was given up). A path is
canonicalised as util-linux canonicalises a mount point; another user's
private volume lists neither its file name nor its bindings, so a miss beside
one says to name it by where it is mounted in `/Storage`. On a healthy volume `--force` still commits cleanly —
nothing is discarded when a clean flush is possible.

The crate is both the pure `no_std` engine library (parser, resolver,
seams — host-tested against in-memory fixtures) and the freestanding
`Run` program (`src/run.rs`), which wires the shared
`tairix_procinfo::IpcTransport`, the `volume_detach` syscall wrapper,
and the `tairix_help::BundleHelp` short-help source over the inherited
standard streams. The kernel is the policy point: the user who attached
a disk image or a network share may detach it, every other volume needs
`CAP_FS_MOUNT`, and every decision is audited kernel-side; the tool holds
no ambient authority. A name two users' disk images share (their file
name) resolves to the caller's own, by the owner each mount record names.

`cargo test -p tairix-unmount` drives the parser, the engine (resolve,
force, refusals, the fd-3 `--force` suggestion), and the
help-document switch pinning across every required locale.
