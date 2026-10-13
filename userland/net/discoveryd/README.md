# `tairix-discoveryd` — link-local service discovery

Stability tier: **experimental**.

`discoveryd` is the multicast DNS / DNS-SD service (`plans/ZEROCONF.md`),
installed at `/System/Services/discoveryd.app/Run` and started at boot once the
network stack is up. It asks and answers questions for clients of
`DISCOVERY_ENDPOINT`; it publishes nothing of its own yet. The service page is
`docs/src/userland/discoveryd.md`.

## Two processes, one binary

* **The front** holds the two multicast DNS sockets, the discovery endpoint,
  and every authority the service has. It never parses a byte a peer sent.
* **The decoder** is this same binary, respawned in the kernel's sandbox spawn
  mode: capability-empty, holding two pipe ends, running one `tairix-net`
  mDNS engine per interface.

## Modules

| Module | Holds |
|---|---|
| `front` | admission, relay, transmit budgets, the acknowledgements a decoder owes |
| `questions` | the shared question table and what each question holds per link |
| `sessions` | client sessions, their queues, owed doorbells and the ports watched for them |
| `query` | the questions a typed request asks, and the authority it needs |
| `grants` | the browse grants read from `/System/Security/Policy/Discovery` |
| `decoder`, `wire` | the sandboxed decoder and the frames crossing to it |
| `events` | the audit event ids |

## Events

`discoveryd` owns ids `25_000..26_000`; the table is on the service page. A
decoder's failure is `lib/sandbox`'s own `6000`.

## Tests

Host tests cover every module, the front over in-process workers including the
real decoder. `fuzz_discoveryd` (enrolled in `cargo xtask fuzz`) holds both
frame codecs canonical, the decoder total over hostile datagrams, the front
whole against a decoder saying anything at all, and the front's tables
consistent — and empty once its clients have gone — across random calls from
many clients against a real decoder. The `test-support` feature exposes the
table consistency checks to that harness alone.
