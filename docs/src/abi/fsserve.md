# Serving a filesystem (`fsserve-v1`)

`fsserve-v1` is how a user-space filesystem driver serves its volume to the
kernel's VFS (`lib/abi/src/fsserve.rs`, `plans/FSSERVE.md`). The kernel's proxy
(`kernel/core/src/fs/served.rs`) is the client and the driver is the server;
the driver's side is the shared runtime in [`lib/fsserve`](../lib/fsserve.md).
How a removable volume comes to be served is
[Volumes served from user space](../filesystem/served.md); a network
filesystem server serves its volumes the same way, offered to one principal
([Network volumes](#network-volumes)).

## Transport

Each served volume has one call endpoint and one shared data window, both made
by the kernel when it accepts the driver's offer (`volume_serve`):

- The endpoint id is in the served range (`SERVED_ENDPOINT_BASE`, one fresh id
  per accepted serve, never reused in a boot). No process can create an
  endpoint there, and a post to one needs a per-endpoint grant that is never
  minted, so the only caller is the kernel. The driver owns it: its end tears
  the endpoint down, which is how the kernel learns the driver is gone.
- The window is `FSSERVE_WINDOW_LEN` (256 KiB), mapped into the driver and
  held by the kernel. Only file data crosses it, at the `Span` a `ReadAt` or
  `WriteAt` names; every other request and reply travels inline.

A network volume, which other writers change, is served by several *lanes*
(`Lanes`): up to `SERVED_LANES_MAX` (16) one-call proxies over the shared
endpoint, each moving data through its own span of the window, a power of two
from `LANE_SPAN_MIN` (16 KiB) to `FSSERVE_WINDOW_LEN`. An operation takes any
free lane, so no caller waits behind another's round trip, and the window is
the lanes' spans end to end, at most `FSSERVE_WINDOW_MAX`. A local volume has
one lane.

## Frames

| Frame   | Header                                                         |
|---------|----------------------------------------------------------------|
| Request | version (2), operation (1), flags (1: the `System` allocation class) |
| Reply   | version (2), operation (1), server state (1), status (4: `0` or a `DriverError` code) |

The server state carries two bits on every reply: the driver holds writes it
has not published, and the volume is in its reserve. The first is what the
kernel counts as lost if the medium leaves.

The operations are those of the filesystem driver traits, numbered from 1 in
this order, so a served volume answers exactly what an in-kernel driver would:
`NodeInfo`, `Lookup`, `Parent`, `ReadAt`, `ReadLink`, `SeekRegion`, `ReadDir`,
`Pin`, `Unpin`, `ReleasePins`, `Create`, `CreateLink`, `Link`, `WriteAt`,
`Truncate`, `Punch`, `Remove`, `Rename`, `Flush`, `Security`, `SetSecurity`,
`RecordsOwnership`, `Stats`, `GetAttr`, `SetAttr`, `ListAttr`, `RemoveAttr`,
and `Release`.

- A create carries the security record the node is born with; a byte write,
  truncate or punch names the node it changes.
- A listing is answered in batches bounded by `FSSERVE_REPLY_MAX`, each ending
  `More`, `Ended` or `Failed`, and resumed from the last name returned.
- `Release` names its lane's span, and answers once the driver has finished
  every earlier request on it. The proxy sends it before it next uses that
  span after abandoning a call, so a driver still working on the abandoned
  call can never write data into another call's bytes.
- A caller being killed, or a driver found stopped, abandons any call. An
  abandoned `Pin`, `Unpin` or `ReleasePins` is not withdrawn: the kernel
  counts each hold it takes and lets go, so the proxy waits the call out
  before it posts the next, and lets go of a pin the driver granted that no
  descriptor now owns. A release it could not send meanwhile stays queued
  for the next operation on the volume.
- The driver's holds die with it. The volume's stand-in then answers every
  descriptor held across the loss with a device fault, and the restarted
  driver serves the volume under a fresh registration, so those descriptors
  answer stale.

Each side decodes the other's frames as untrusted. The kernel holds a reply to
the request it asked: a count no larger than the span, a name that is a path
component, a security record within its bounds, a listing that makes
progress. A reply that fails is a malformed answer: the call fails closed and
the volume reads as degraded until the driver answers cleanly again. The
driver refuses a request the kernel could not have sent.

## Offering a volume

The driver opens its volume and tells the kernel the outcome in one
`ServeFrame` (16 bytes): `Serve` with a `ServeOffer` — the root, the name
rule, how long a node keeps its id, whether the format keeps attributes, why
it will not write the volume if it will not, and the `FormatName` the mount
table shows — or `Refuse` with the `DriverError` the format gave. An accepted
offer is answered with a `ServeGrant` (24 bytes): the endpoint and the
window's address and length.

The kernel holds the offer to its own view of the volume: the format's family
must be the one its probe recognised, and a driver may serve a volume
read-only but never write one the device or the kernel refuses.

## The volume syscalls

A driver reaches its volume, and only its volume, through four syscalls. Each
requires `CAP_FS_SERVE` and a grant covering the `HwResourceKind::Volume`
resource its node carries.

| No. | Name           | Arguments                                         | Returns                |
|-----|----------------|---------------------------------------------------|------------------------|
| 155 | `volume_read`  | volume, first block, buffer, length               | `errno`                |
| 156 | `volume_write` | volume, first block, buffer, length               | `errno`                |
| 157 | `volume_ctl`   | volume, `VolumeCtl` (24 bytes), reply buffer      | reply length           |
| 158 | `volume_serve` | volume, `ServeFrame`, grant buffer                | `SERVE_GRANT_LEN`, or `0` for a refusal |

- A transfer is whole blocks within the volume, at most `VOLUME_IO_MAX` (the
  window's length) per call; anything else is `LengthOutOfRange`.
- `volume_ctl` describes (`Info`, answering a 56-byte `VolumeInfo`: block size
  and count, read-only, discard, an optical disc's layout, the medium),
  flushes, or discards.
- A write to a read-only volume is `ReadOnly`. A volume that has left service
  answers `NotFound` or `DeviceOffline`.

## Network volumes

A network filesystem server holds `CAP_FS_SERVE` and is admitted from a signed
bundle; it serves no volume grant. It offers each volume to the one principal
that may attach it, and the kernel mints the endpoint and the window as it
does at `volume_serve`.

| No. | Name             | Arguments                                         | Returns          |
|-----|------------------|---------------------------------------------------|------------------|
| 159 | `fsserve_offer`  | `NetworkOffer`, grant buffer                      | `OFFER_GRANT_LEN` |
| 160 | `fsserve_attach` | offer, `NetworkAttach`, reply buffer              | `ATTACH_REPLY_LEN` |
| 161 | `fsserve_ctl`    | offer, `FsserveCtl`                               | `errno`          |

- A `NetworkOffer` (`lib/abi/src/netattach.rs`, at most `NETWORK_OFFER_MAX`)
  is a `ServeFrame` over a network `FormatName` (`smb3`), the uid it is for,
  its lanes, a 16-byte key, and the source the mount table shows
  (`//host/share`). It is answered with an `OfferGrant` (32 bytes): the
  `ServeGrant` and the `OfferId`, never zero and never reused in a boot. An
  offer mounts nothing. Only a server the kernel admitted from a signed bundle
  may make one: the volume's identity is derived from the server's attested
  application identity, the principal and the key, so a server never names a
  local volume, or another server's.
- `fsserve_attach` is called by the principal the offer names; to anyone else
  the offer does not exist (`NotFound`). A `NetworkAttach` names the volume (a
  base name the kernel suffixes through the shared naming policy until one is
  free), its presentation, a narrowing to read-only, and optionally a target
  it is also bound at. *Private* — owner the caller, its primary group,
  `rwx------` and `rw-------` — is anyone's; *shared* (the storage group,
  `rwxrwx---`, `rw-rw----`) and *explicit* (an owner, group and modes held to
  the permission bits) need `CAP_FS_MOUNT`. Every node is presented so,
  whatever owner the server claims to record, and no security record is
  stored through it; a node the server marks read-only loses its write bits. The volume appears at `/Storage/<name>`, `noexec`; the
  reply (56 bytes) is its identity and the name it took. A target refused
  leaves the offer to be attached again.
- `fsserve_ctl` is the server's word on an offer it made: `Alive` while calls
  it is working on are outstanding — a network server touches no volume grant,
  so this is what keeps a slow call from being abandoned after
  `SERVED_IDLE_NS` — and `Withdraw`, which retires the offer or its volume
  without waiting on the server's own lanes. Another process naming the offer
  is answered `NotFound`.
- `volume_detach` retires an attached network volume for its owner, or any
  caller holding `CAP_FS_MOUNT`, and takes its endpoint down; the server
  learns the volume is gone by its endpoint going. A volume whose server is
  withdrawn or lost leaves the namespace at once, unless the server last said
  it held writes it had not published: that one stays, reporting
  `UnavailableLost`, until its owner detaches it with `force`.
