# Volumes served from user space

Every filesystem on removable media — ARXFS, ext2/3/4, FAT32, ADFS, ISO 9660
and UDF — runs in a user-space driver process, not in the kernel
(`plans/FSSERVE.md` FSS5). The kernel keeps only the drivers of the volumes it
boots from: ARXFS for the root and `/System`, FAT32 for the boot partition. A
format the kernel has never heard of is served by shipping its driver bundle.

## What stays in the kernel

The disk stack: the block-service client, the device's cache, the
uncommitted-write journal, the surprise-removal transitions and the verified
re-insert replay ([Storage namespaces](./drives.md) §10). A driver never sees
the device, its partition table or a sibling partition: it reaches only its
own volume's extent, through the volume grant, in whole blocks, with every
write passing through the journal exactly as an in-kernel driver's did.

The kernel names the volume. Its identity, the family of its format and its
catalog name come from the kernel's own probe (`lib/fsprobe`) and the volume
manager's request, never from the driver, so no driver can claim another
volume or present itself as a different format. A driver may ask for read-only
— and an ISO 9660 driver always does — but never for writes the device or a
re-insert's conflict forbids.

## Attach and serve

1. The volume manager asks for the attach (`volume_attach`), as for any
   volume. The kernel connects the device, windows the extent, recognises the
   volume, reserves its catalog name, and builds the served volume: the object
   the grant names.
2. The kernel publishes a volume node under the storage node the volume
   manager was loaded for: class storage, compatible
   `tairix,volume-<format>`, one `HwResourceKind::Volume` resource. Nothing is
   mounted yet (`fs.serve.awaiting`).
3. `devmgr` matches the node like any other and loads that format's bundle
   (`/System/Drivers/filesystem/<format>/Run`) holding exactly the node's
   grant. Its capabilities are `CAP_FS_SERVE` and `CAP_LOG_EMIT`; it needs no
   endpoint or shared-memory authority, because the kernel makes both for it.
4. The driver describes its volume (`volume_ctl`), opens the format, and
   offers it (`volume_serve`). The kernel makes its endpoint and data window,
   builds the proxy, mounts it at `/Storage/<name>` with the removable-media
   flags (`noexec`, plus `ro` when the device, a conflict or the driver says
   so), and publishes its identity (`fs.hotplug.root_added`). A format the
   driver cannot open is refused with its reason, and the volume is retracted
   (`fs.serve.refused`).

The protocol is [`fsserve-v1`](../abi/fsserve.md).

## A served volume in use

The proxy is one more filesystem driver in the mount registry, so a served
volume keeps everything a removable volume had: the kernel's cache in front of
it, the directory-watch feed, per-mount serialisation, and for FAT32, ADFS,
ISO 9660 and UDF the storage-group identity map. Every call waits only while
the driver is answering or working on its volume; a driver that does neither
for ten seconds has stopped, and the call is abandoned. A kill abandons a call
too (`Errno::Interrupted`). An abandoned call leaves the window quarantined
until a `Release` round trip proves the driver done with it; an abandoned
pin, unpin or release of every pin is not withdrawn but waited out before the
next call, and a pin the driver turns out to have granted is let go, so the
kernel never loses count of a hold. A driver once found stopped fails each
later call at once until it shows life again.

A driver that batches commits (ARXFS, ext4) holds its open transaction in its
own process, bounded by the machine's memory and its pressure band, and
publishes it on its own commit timer. A descriptor's last release is handed
over by the next operation on the volume, the close that dropped it, or a
sync, which hands them over before it flushes; never by the kernel's
write-back flusher, which serves every volume and so never waits on a
driver. Every reply says whether the driver holds unpublished
writes.

## When things go

- **Detach.** The kernel hands the driver the releases waiting, flushes
  through the proxy, commits the device, unmounts, and puts the fail-closed
  stand-in in the driver's place; then it retires the served volume,
  withdraws the node (so `devmgr` unloads the driver), and unregisters and
  unpublishes the volume. A volume no driver serves has no filesystem state
  to flush. A plain detach is refused while a descriptor holds one of the
  volume's nodes; a force detach has the driver let every hold go first.
- **Unplug.** The surprise-removal transitions apply unchanged; writes the
  driver reported unpublished count as lost. The node and the served volume
  go with the device.
- **Driver lost.** The stand-in answers every call, the mount reports
  `Recovering`, and the node stays published, so the driver `devmgr` restarts
  serves the same volume again (`fs.serve.lost`, then `fs.serve.resumed`).
  Writes it held unpublished are lost with it, and the record says so. Its
  holds go with it too: a descriptor held across the loss answers the device
  fault while the volume waits, and stale once the restarted driver serves it.
- **Re-insert.** The retained writes are replayed at the block layer, then a
  fresh served volume and node are published; the stand-in answers the mount,
  under its new posture, until the new driver serves it.
- **Refusal of a conflicted re-insert.** A volume whose non-mutation could not
  be proven keeps its retained set for the audited force-unmount, so a driver
  that will not serve it leaves it mounted read-only behind the stand-in
  rather than retracted.

## Events

| Id   | Name                     | Meaning |
|------|--------------------------|---------|
| 4192 | `fs.serve.awaiting`      | attached, its node published for its driver |
| 4172 | `fs.hotplug.root_added`  | first served, mounted and published |
| 4193 | `fs.serve.resumed`       | served again after a lost driver or a re-insert |
| 4194 | `fs.serve.refused`       | not served (`cause`: `driver_refused` or the kernel's refusal); retracted, or kept when conflicted |
| 4195 | `fs.serve.lost`          | its driver is gone (`cause`: `unpublished_writes_lost` or `nothing_unpublished`) |
