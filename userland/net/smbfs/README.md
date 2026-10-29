# `tairix-smbfs` — the network-share service

Stability tier: **experimental**.

`smbfs` lists, mounts and serves SMB 3 shares (`plans/SMB.md`). It is
installed at `/System/Services/smbfs.app/Run`, enrolled at boot under its own
`smbfs` account, and answers `netshare-v1` on `NETSHARE_ENDPOINT`, whose
callers the kernel holds to `CAP_NET`. The service page is
`docs/src/userland/smbfs.md`.

## Processes, one binary

* **The front** holds the endpoint, the sockets, the offered volumes and every
  authority the service has. It parses no SMB.
* **One worker per connection** is this same binary, respawned in the
  kernel's sandbox spawn mode: capability-empty, holding two pipe ends, running
  one `tairix-smb` client.

## Modules

| Module | Holds |
|---|---|
| `front` | the calls, the connections they share, the mounts they made, the volumes it serves and what it caches of them, deadlines |
| `nodes` | a served volume's node ids, keyed by the share's folded path |
| `link` | one connection's worker session, its waiting orders and its socket backlog |
| `mounts` | the mount table, the login sessions it ends with, the volume key |
| `judge` | what each error a worker reports answers a caller |
| `host` | the seam the front runs over |
| `worker`, `files`, `wire` | the sandboxed worker, the opens it keeps for the front, and the frames crossing to it |
| `events` | the audit event ids |

## Events

`smbfs` owns ids `35_000..36_000`; the table is on the service page. A
worker's failure is `lib/sandbox`'s own `6002`.

## Tests

Host tests cover every module, the worker against `tairix-smb`'s scripted
server and the front over in-process workers whose sockets reach that server.
`fuzz_smbfs` (enrolled in `cargo xtask fuzz`) holds both frame codecs
canonical and total, and drives the front and its served volumes against
perturbed workers and servers. `expiry_allocation` holds a volume's call to
being answered at its deadline with the allocator refusing.
