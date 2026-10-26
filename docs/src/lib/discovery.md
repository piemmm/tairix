# `tairix-discovery` — the link-local discovery client

`lib/discovery` is how a program browses for, resolves, and looks up link-local
services. It never speaks multicast DNS itself: the network stack reserves the
port and groups to [`discoveryd`](../userland/discoveryd.md), which asks the
segment for every client and admits each request against the caller's
kernel-attested identity. Stability tier: **experimental**.

## Shape

- **`Session`** — the pure client over an injected `Transport`. `open` names
  the private port the service rings; `start` begins a typed request
  (`Query`: browse, resolve, host, reverse, or every type) and returns its id;
  `collect` takes whatever has arrived, never waiting; `stop` ends one request
  and `close` (or dropping the session) ends them all. Requests are answered
  continuously until stopped.
- **`Held`** — one request's current answers, kept from the entries `collect`
  returns. Each entry says an answer was added, renewed, or retired, that
  everything held on one interface is void, or that updates were lost for
  want of room to queue them; `apply` folds one in and reports what moved, so a
  browse that runs for an hour costs what changes rather than what is held.
- **`RtDiscovery`** (the `program` feature) — the production glue: the endpoint
  call, and a wait on the session's doorbell that the service wakes. A
  doorbell is believed only when its kernel-attested sender is the service's
  account and it names this session.
- **`host_addresses` / `reverse_name`** — one-shot lookups bounded by a
  deadline, which `lib/resolver` routes every `.local` name and link-local
  address to.
- **`walk::Discovery`** — the requests a program keeps open to follow what the
  segment offers. From the roots it names — the type list, a browse, the
  computers question, the name of an address — it follows what each holds: a
  browse per type, a resolve per instance, a lookup per `.local` host an
  instance resolves to, each kind held to the program's `Bounds`. It
  reconciles the open set against what is held once per collect, so a type
  that goes takes everything below it; a request whose updates were lost is
  started again before the old one stops, unless the account is at its bound,
  and one lost again within its wait — the program's pace, doubling — is held
  back. An ask refused for authority, or one the service will not answer, is
  not made again while wanted. It returns what peers sent, raw: `Service`s,
  `Computer`s, and the names hosts and addresses were given.
  `walk::Collect` and `walk::OpenSession` are the reader's collect state and
  the doorbell match for a program that collects on a thread of its own.
- **`catalogue::Catalogue`** (`plans/ZEROCONF.md` Z8) — the storage the segment
  offers, as one list of endpoints: the instances of each storage type
  (`STORAGE_TYPES`, `_smb._tcp` — the share protocols this system mounts), the
  computers WS-Discovery names, and the hosts the user names. An instance and a
  computer at one address on one link are one endpoint, named by the instance
  and carrying the computer's workgroup or domain. At most 256 endpoints are
  listed, at most four from one source on one link, the rest counted against
  their source in `Listing::omitted` rather than dropped silently, and at most
  32 hosts named. It is a query, never a path, and raises no credential
  prompt: a choice carries its `Endpoint` to the mount. `files.app`'s Network
  view lists it (Z9), through `lib/browse`'s network location.

## Guarantees

- Every answer names the interface it was learned on; nothing merges two links.
- Every name in an answer was written by an unauthenticated peer: structurally
  valid, never display-safe. A renderer shows it through the one display-safety
  policy (`tairix_util::utf8::shown`) or escapes it
  (`tairix_net::dns::Presentation`).
- Resolution returns an address, never a capability.
- Buffers are committed fallibly at open, so a session that opens never
  allocates on its answer path.

## Errors

`DiscoveryError` separates the service being absent (`Unavailable` — a build
without discovery, or one where the service has not started) from a refusal
(`Refused`, carrying the service's `Errno`), a transport failure, and a reply
that does not decode (`Malformed`).

## Tests

The pure layer is host-tested over a fake transport: the session lifecycle and
its close on drop, an absent service and a refusal, `Held`'s folding of every
edge, a flush, and a loss; the walk's descent, unwinding, replacement and
backoff, bounds and refusals; and the catalogue's roots, its merge of an
instance and a computer per address and link, the per-source quota and its
count of what it held back, named hosts, and a flushed link. `dns-sd` and `lib/resolver` exercise it end
to end, and the `tairix-test-discovery-qemu-aarch64` vertical runs it against
the live service.
