# `tairix-fsserve`

The runtime every user-space filesystem driver serves its volume through
(`lib/fsserve`, `plans/FSSERVE.md` FSS2). The protocol is
[`fsserve-v1`](../abi/fsserve.md); how a volume comes to be served is
[Volumes served from user space](../filesystem/served.md).

## A driver supplies its format

A format's driver crate implements `ServedFormat`: open the volume it is
given, only for reading when the kernel says so, with its write-back bounded
by a `WritebackBudget` (the machine's memory and a pressure gauge) and its
diagnostics recorded on the log the runtime lends it (`Sink`, the process's
own log under `CAP_LOG_EMIT`), and name
what it opened — its `FormatName`, down to the generation, and why it will not
write the volume if it will not. `Opened::offer` turns that into the
`ServeOffer` the kernel is given, saying the volume keeps a title unless its
driver's `label` answers `Unsupported`. Its bind table is `bind_key(fstype)`, the
compatible of the volume nodes the kernel publishes for that format, and every
served driver runs with `REQUIRED_CAPABILITIES`: `CAP_FS_SERVE` and
`CAP_LOG_EMIT`.

An archive's driver implements `ServedArchive` instead: it is handed the
attachment itself, reads the archive by byte (`VolumeAccess::region_read`),
and writes only its work files — `region_write`, and `ctl` for the copy
between regions the kernel makes itself, a truncate, and the commit that
makes the next archive the archive ([Disk images](../filesystem/images.md)).
`program::run_archive::<F>()` is its `main`. With the `test-util` feature,
`MemoryAttachment` is such an attachment in memory, for an archive driver's
tests: it holds the archive and both work files, refuses what the kernel
refuses, and counts the copies, writes and commits a driver asked of it.

## The volume, as a block device

`GrantedVolume` is a `Block` over `VolumeAccess`, the kernel's calls on the
granted volume. It takes its geometry, discard capability, medium and disc
layout from the kernel's `VolumeInfo`, and splits a transfer into calls of at
most `VOLUME_IO_MAX` bytes of whole blocks. It refuses a sensitive payload:
the kernel stages a served volume's transfers in buffers of its own, which
such a payload may not be left in.

## Answering the kernel

`Server::answer` decodes one request, runs it against the driver, and encodes
the reply with the state `ServedHost` holds. File data moves through a
`Window`, which lends a call only its own span: the kernel stages and collects
the other lanes' spans meanwhile, so nothing borrows the whole window. A
request the kernel could not have sent is refused without reaching the driver,
and a listing entry no path could name fails the listing closed, as it does
from an in-kernel driver.

`ServedHost` is the driver's `VolumeHost`: it keeps the commit deadline and
the reserve the driver reports. `take_due` takes a deadline once it falls due,
so a publish that fails leaves its transaction open for the next operation and
never re-arms the timer in the past; whether writes are held is reported
apart from the deadline, so a reply after a failed publish still says so.

## The driver process

With the `program` feature, `program::run::<F>()` is every served driver's
`main`: it reads the node's grant, describes and opens the volume, offers it
(or refuses it with the format's reason), and then serves from one wait set —
the endpoint the kernel made for it, the memory-pressure notice its write-back
bound reads, and the commit deadline as the wait's timeout. It ends in the driver exit vocabulary ([`tairix-drvrt`](./drvrt.md)), so
`devmgr` restarts a driver that failed while serving and never one whose
volume would not open ([Hardware detection](../drivers/hardware-detection.md)).
