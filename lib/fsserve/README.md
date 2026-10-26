# tairix-fsserve

The server side of `fsserve-v1`: what a user-space filesystem driver links to
serve its volume to the kernel (`plans/FSSERVE.md` FSS2).

- `Server` answers one kernel call at a time against the filesystem driver
  traits, so every format shares one dispatch of the protocol.
- `ServedHost` is the `VolumeHost` the driver reports its commit deadline and
  its reserve to; each reply's server state is read from it, and its deadline
  fires once.
- `GrantedVolume` is the volume a driver's grant names, as the `Block` device
  its format is opened over, through `VolumeAccess` — the kernel's
  `volume_read`, `volume_write` and `volume_ctl`.
- `ServedFormat` is all a format's driver crate supplies: how it opens a
  volume, read-only or with its write-back bounded by a `WritebackBudget`.
  `bind_key` and `REQUIRED_CAPABILITIES` are every served driver's bind key
  and capability set.
- The `program` feature's `program::run` is the whole driver process: read
  the grant, open, offer or refuse, then serve the endpoint, the commit timer
  and the memory-pressure wake from one wait set. It links the userland
  runtime, so it is built only for a bare-metal `Run` binary.

`docs/src/lib/fsserve.md` describes it; the protocol is
`docs/src/abi/fsserve.md`.

## Stability tier

`experimental` — tracks `fsserve-v1` in `lib/abi`. `no_std` with `alloc`, no
`unsafe`.
