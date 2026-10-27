# tairix-netshare

TAIRiX network-share client (`plans/SMB.md` SMB8). Stability tier:
**experimental**.

A program lists an endpoint's shares, has one mounted, withdraws a mount, and
lists its own mounts through the `smbfs` service with this crate, over the
`netshare-v1` protocol (`tairix_abi::netshare_ipc`). The pure `Client` runs
over an injected transport and is host-tested; the `program` feature adds the
production transport that calls the service's endpoint. A mount is answered
with an offer the caller attaches itself.

See `docs/src/lib/netshare.md`.
