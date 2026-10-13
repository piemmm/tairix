# NETMAP.md — `netmap.app`, the network map and inventory

Staged build plan for TAIRiX's first-class network discovery, inventory and
topology-mapping application, and the read seams it needs from the network
stack. **Binding under `AGENTS.md`** — read it first (especially §2, §4, §5,
§10, §16, §19, §24, §26, §28). It consumes the seams `plans/NETWORK.md`,
`plans/DNS.md` and `plans/ZEROCONF.md` fix and never contradicts them; where
they touch, NETWORK.md's and ZEROCONF.md's decisions stand. `abi-v1` is **not**
frozen (PLAN.md Stage 1), so the ABI the seam increments add and the in-place
evolutions they make are ordinary pre-release changes (§2.13).

The read seams (neighbour cache, routes/gateway, the traceroute ICMP-error
path) are *stack* surface and so are governed by `plans/NETWORK.md`'s rules;
they live here because they exist to serve this app and have no other consumer
today. `plans/NETWORK.md` §5 already *promises* `state:net/<iface>/routes`;
NM-S2 is where that promise is kept. Each seam lands with its consumer, never
ahead of it (§2.4).

## Ledger

| id | What it is | Status |
|---|---|---|
| NM1 | The pure `tairix-netmap` engine: the complete map model (every node/edge kind, the device-class/OS-guess field), ingest from the landed interface facts/state, the deterministic radial layout over the full model, the graph-space viewport (zoom/pan/anchor) | done |
| NM2 | The GUI app: the `Run` binary, the window, the map renderer drawing the device glyphs, pointer/scroll/pinch routing, the System Information reads on a worker off the loop, the bundle (manifest, 256² icon master, Help×13), docs | done |
| NM2b | Advertised services and names on the map: the discovery-v1 browse/resolve ingest (`Session` on the app's own port, `CAP_NET` + `CAP_NET_DISCOVER_ALL`) with peer-authored labels escaped, and reverse DNS for host labels | done |
| NM2c | The live QEMU vertical: open `netmap` from the Library on the graphical desktop with a router-advertising link peer, and assert the interface, subnet and gateway tiers against a screendump | done |
| NM2d | Services folded into their host: no service is drawn until asked for — a host wears a count of what it offers and names it in its tooltip, a click spreads its services around it (animated) and folds them again, and a secondary press opens the node's or the map's context menu through the desktop's one menu chain (show/hide services, copy address, name, hardware address, network and URL, fit the map) | done |
| NM2e | *Open in Browser* on an `http`/`https` service's context menu: the service's URL handed to the application that opens its scheme | blocked — TAIRiX ships no web browser, and the desktop has no URL hand-off (a scheme a bundle declares it opens, and an open request carrying a URL); both lie outside this plan |
| NM-ICONS | The first-class device-class icon vocabulary (desktop, laptop, server, router, switch, access point, printer, phone, NAS, IoT, the local host, unknown) in `lib/icon`, shared and scalable, drawn by the map and reusable desktop-wide | done |
| NM-S1 | Neighbour-cache read seam (ARP + NDP): `lib/net` iterator, the `NetNeighborRecord` ABI + `NET_NEIGHBORS` query, netstack serve, `lib/procinfo` resolver + `state:net/<iface>/neighbors`; engine L2-adjacency ingest | done |
| NM-S2 | Routing-table + gateway read seam, and router discovery: `NetRouteRecord` ABI + `NET_ROUTES` query, netstack serve, `lib/procinfo` resolver + `state:net/<iface>/routes` (keeps NETWORK §5's promise), the RA-learned IPv6 default routers; engine router/gateway ingest | done |
| NM-F | Passive device-class and OS inference: the pure inference engine over the inventory's signals (routing role, MAC OUI vendor, advertised service types and TXT model), the committed IEEE MA-L OUI vendor database with its `cargo xtask` generator and drift gate, and the OS emblems a host is marked with | done |
| NM3 | Active reachability probing, configurable and polite: the pure probe engine (token-bucket rate limit, bounded concurrency, injected socket+clock seam), the ICMP-echo subnet sweep and the TCP-connect well-known-service check, the reply-TTL and open-port evidence they add to NM-F's inference, the audit events, and the settings pane that holds the posture | in progress: reply-TTL seam landed, probe engine/inference/settings/audit/fuzz remain |
| NM-S3 | The traceroute seam: per-send hop-limit control and ICMP-error delivery to the owning socket (`lib/net` quoted-packet demux, netstack routing, `lib/rt` wrappers), plus the `traceroute` command app and its QEMU vertical as the minimal consumer | done |
| NM4 | Inter-network topology: engine hop-by-hop path discovery to user-entered targets (LAN or WAN) over NM-S3, per-hop reverse DNS, and the "beyond the gateway" map region | planned |
| NM-S4 | The passive packet-capture seam: a receive-only, capability-gated, bounded link-layer capture socket from netstack, and the driver capture tap upstream of the receive pre-filter so non-IP ethertypes (STP, LLDP, …) reach it; `lib/rt` wrappers; audited | planned |
| NM-S5 | Layer-2 topology from capture: the sandboxed STP/LLDP/CDP parsers (`lib/net` engines behind a §19.5 `lib/sandbox` capture-decode worker), feeding `Switch`/`AccessPoint` nodes and `Bridged` port edges — switch diagramming, best-effort | planned |
| NM5 | UX completion: node detail panel, search/filter, service-type badges, liveness/freshness, keyboard navigation, the map legend, the complete settings pane, the README currency review | planned |

The ledger above is the one status record (§13); the prose states design and
invariants, never status.

## 0. Scope and decisions (binding)

- **It is an administrator's inventory and map tool, not an attack tool.** The
  design goal is the best-in-class equivalent of a friendly host-discovery
  sweep, a neighbour/route inventory, and a topology map — what an operator
  runs against a network *they administer*. It deliberately contains **no**
  stealth/evasion machinery, no half-open/SYN scanning, no OS-fingerprinting
  by malformed packets, no exploitation, and no unsolicited sweeping of
  networks the host is not attached to. These are not first-class-OS features;
  they cut against the charter's security posture (§5, §19) and are absent
  rather than stubbed (§2.3/§2.4). The "clever exploration of less-local
  networks" is principled route/gateway inference and operator-directed
  traceroute, not uninvited probing of third-party networks.
- **Reconnaissance is an administrative class, and netmap is gated as one.**
  Enumerating a whole segment — every neighbour, every advertised service, the
  routing table — is the reconnaissance class the charter already gates behind
  `CAP_SYSINFO_GLOBAL` / `CAP_NET_DISCOVER_ALL` (ZEROCONF §5; the
  `NET_SOCKETS` precedent). netmap requests those administrative capabilities;
  the kernel grants only their intersection with the launching user's grants
  (§4 — no ambient authority), so an unprivileged user gets a strictly
  narrower tool, never a backdoor.
- **Fail loud, degrade gracefully (§2.24).** Every data source is optional and
  independent. A source the caller's grants refuse (no `CAP_SYSINFO_HW`, so no
  interface MACs; no `CAP_NET_DISCOVER_ALL`, so no service browse) is reported
  in the app's own UI and omitted from the map; the session continues with the
  authority it has and never crashes, fabricates a result, or exits over a
  refused *optional* layer. A failure that defeats the app's primary purpose
  (no window) is fatal and states its reason on `stderr`.
- **Active probing is configurable, defaulting to polite, and always
  bounded.** The settings pane holds a posture — `passive`, `polite` (the
  default), `thorough` — plus the rate and concurrency knobs; it is persisted
  in the app's own app-data store (§16.3) and read at start, never written
  from the interactive loop (§28). Every probe campaign is rate-limited by a
  token bucket, bounded in concurrency, bounded in target set (sweeps cover
  only subnets this host is attached to; ping/traceroute to a single
  user-entered host may target any address, LAN or WAN, because it is
  operator-directed), fails closed on refusal, and is audited through
  `lib/log` with a stable event id (§5.4, §19.4). Choosing our own CPU/wire
  cost is a denial-of-service channel, so the bounds are structural, not
  cosmetic (the ZEROCONF §8 / NETWORK N17 precedent).
- **Honours the permanent network non-goals.** No LLMNR, no NetBIOS name
  service — ever (ZEROCONF §1); netmap never speaks them. No off-link mDNS
  responses and no amplification (ZEROCONF §1, §8). `.local` stays on the link
  (ZEROCONF §1).
- **Host-type and OS inference is *passive*, from the inventory already
  gathered.** The map labels each host with a best-guess device class and OS
  family and draws a matching first-class icon — but the guess is drawn only
  from signals netmap *already has*: the reply hop-limit/TTL banding, the open
  well-known ports the reachability check found, the service types and TXT
  attributes a host advertises, and the MAC OUI vendor. netmap sends **no**
  crafted or malformed probe to tease out an OS (the nmap `-O` technique); that
  is the aggressive, impolite fingerprinting the posture excludes. A guess is
  always presented *as* a guess, with a confidence, never asserted as fact —
  peer-supplied signals are untrusted (§5), and a wrong icon must never read as
  ground truth.
- **Routers and switches are discovered, not assumed.** A router is identified
  from the routing table's gateways (NM-S2), from ICMPv6 Router Advertisements
  the stack's ND already processes, and from traceroute hops (NM4). A switch is
  identified, best-effort, from the layer-2 control frames a passive capture
  sees (NM-S5) — LLDP/CDP carry a neighbour switch's identity and port
  directly, and STP's bridge/root IDs reveal the spanning-tree shape. Where the
  network filters those frames or the host sits on an access port that reveals
  nothing, the switch simply does not appear (§2.24 — absence is not an error);
  the feature is honestly best-effort, never fabricated.
- **Packet capture is passive, receive-only, capability-gated, and bounded.**
  The capture seam (NM-S4) is the pcap/tcpdump-grade facility every serious OS
  ships for diagnosis, built defensively: it only *observes* — it never injects
  a frame — it is gated on `CAP_NET_RAW` (administrative), every captured frame
  is hostile input parsed only inside a §19.5 minimum-capability sandbox
  (NM-S5), the capture ring is bounded and drops oldest under pressure rather
  than growing, and enabling a capture is an audited event (§19.4). It widens
  what *a capture subscriber* sees without changing the stack's own data path:
  the normal receive filter still sheds non-IP ethertypes for the stack's own
  processing; the tap copies them only while a capability-holding capture
  socket is live on that interface.
- **One engine, pure and host-testable.** The whole map — the model, every
  ingest, the layout, the viewport, the hit-test, and the probe campaign
  scheduling — lives in the crate's `no_std` + `alloc`,
  `#![forbid(unsafe_code)]` library, driven through injected seams (the typed
  records, a clock, an echo/stream-socket transport). The `Run` binary is thin
  glue over `tairix-rt`. The tested code and the shipped code are the same code
  (the view/paint/telnet precedent, §2.2).
- **Untrusted input stays untrusted.** Every byte the map draws from the
  network — a service instance label, a TXT record, a reverse-DNS name, a
  router's quoted address — was authored by an unauthenticated peer. It is
  structurally validated at the ABI boundary (the discovery/DNS codecs already
  do this), never display-safe, and is escaped before it reaches a glyph
  (the `Presentation` discipline `dns-sd` already uses). netmap decodes no raw
  wire format itself: discovery, DNS and sysinfo all return typed records from
  trusted system services, so netmap needs no parser sandbox of its own.
- **Event-driven, never polled (§2.23, §28).** The app parks on one wait-set
  holding the window event mailbox, the worker-desk wake, the discovery
  session's private port, and the animation/probe one-shot deadline. A probe
  campaign's pacing is a one-shot timer re-armed to the next token, never a
  spin. Nothing busy-waits.
- **Not in this plan:** the `files.app` Network *catalogue* view and the
  discovered-share mount into `Storage:` (that is ZEROCONF Z8/Z9, gated on the
  §6.6 display-safety policy); service *publication* (ZEROCONF Z5); any
  firewall/NAT/forwarding control (NETWORK §9); SNMP and SSDP inventory, and
  transmitting LLDP — the map listens to the LLDP its neighbours send (NM-S5)
  but never speaks it (each would need its own stack surface and is a later
  plan, §2.3); and WAN
  port-scanning of networks the host does not administer (out of scope by
  design, above).

## 1. The map model (binding)

The map is a graph in a fixed integer **graph-space** coordinate system,
rendered through a viewport. The model is deliberately small and total.

- **`Node`** — one discovered thing. `NodeId` is a stable, content-derived
  identity so re-ingesting the same source does not duplicate or churn a node
  (an interface by its name, a host by its primary address, a service by its
  instance+type+interface, a router/switch by its address or bridge id). The
  `NodeKind` closed set is `LocalHost`, `Interface`, `Subnet`, `Neighbour`,
  `Service`, `Router`, `Gateway`, `Switch`, `AccessPoint`, `RemoteHost`,
  `Beyond` (the "rest of the network / WAN" sink a default route points at).
  Each node carries its kind, its identity, a display label (escaped), the
  addresses and facts it has accrued, a liveness/freshness stamp, and a
  **`Classification`** — the passive device-class + OS-family guess with its
  confidence (NM-F), which is what selects the node's first-class icon. The
  classification defaults to `Unknown` so a node always has a meaningful icon
  before any inference runs (§2.9 — never a blank).
- **`Edge`** — one relationship. `EdgeKind` is `Hosts` (host→interface),
  `OnLink` (interface→subnet / subnet→neighbour), `Serves`
  (host/neighbour→service), `Routes` (interface→router / router→subnet),
  `Default` (interface→gateway→beyond), `Bridged` (switch↔switch / switch↔host
  port adjacency from layer-2 capture, NM-S5), `Hop` (traceroute path segment).
  Edges are deduplicated by (from, to, kind).
- **Ingest is additive and idempotent.** Each source contributes nodes and
  edges keyed by identity; re-running a source updates facts and freshness in
  place and retires what a flush/retire event withdrew (the discovery
  `Change::{Added,Refreshed,Retired}` and `Flush` discipline). No source
  clears another's contribution.
- **Layout is deterministic and pure.** A tiered radial layout places the
  local host at the origin, its interfaces around it, each interface's subnet
  beyond it, neighbours and services on each subnet's ring, and routers /
  gateway / beyond on the outward tier; hop paths extend outward from the
  gateway. Given the same graph the layout is byte-identical, which is what
  makes it testable and the viewport reproducible. It is not a force-directed
  simulation (non-deterministic, unbounded, hard to test); a map wants a
  stable, legible arrangement, not jitter. A service is not on a ring: it is
  folded into its host until the user spreads it (NM2d).
- **The viewport** maps graph-space to window-space through an integer zoom
  (per-mille, clamped to a documented range) and a pan offset, with
  anchor-preserving zoom (`zoom_about(factor, anchor)`) so a pinch or
  Ctrl+wheel keeps the point under the pointer fixed — the exact contract
  `userland/apps/view`'s `Viewport::zoom_about` keeps, re-expressed for graph
  coordinates (the arithmetic is the same *shape*, but the space differs, so
  it is a sibling, not a shared type). `pan_by` and `clamp_pan` bound the pan
  to the laid-out extent plus a margin. Hit-testing is the view's, against the
  plates it draws (NM2d).
- **All bounds are fixed security/containment bounds, not capacities a machine
  scales (§24.4):** the maximum node and edge counts, the maximum label
  length, the maximum hops a path holds. A source that would exceed one is
  truncated with a visible "N more" marker (the ZEROCONF "N more from this
  host" precedent), never grown without bound — an attacker on the segment
  authors these, and our memory cost is their denial-of-service channel.

## 2. Ingest sources

| Source | Reached through | Capability | Contributes |
|---|---|---|---|
| Interfaces (facts) | `NET_INTERFACE_FACTS` via `tairix_procinfo::for_each_net_interface` | `CAP_SYSINFO_HW` | `LocalHost`, `Interface` nodes; MAC/MTU/kind facts |
| Interfaces (state) | `NET_INTERFACE_STATE` via the sysinfo walk | `CAP_SYSINFO_GLOBAL` | interface addresses, link state, `Subnet` nodes + `OnLink` edges |
| Advertised services | `discovery-v1` browse/resolve (`Session<RtTransport>`) | `CAP_NET` + `CAP_NET_DISCOVER_ALL` | `Service` nodes, `Serves` edges, host hints |
| Names | forward/reverse DNS via `tairix_resolver` | `CAP_NET` | host labels, reverse names |
| Neighbours (NM-S1) | `NET_NEIGHBORS` via `lib/procinfo` | `CAP_SYSINFO_HW` | `Neighbour` nodes + `OnLink` edges (L2 adjacency) |
| Routes (NM-S2) | `NET_ROUTES` via `lib/procinfo` | `CAP_SYSINFO_GLOBAL` | `Router`/`Gateway`/`Beyond` nodes + `Routes`/`Default` edges |
| Reachability (NM3) | ICMP echo + TCP connect (`tairix_rt::net`) | `CAP_NET` + `CAP_NET_RAW` | liveness on hosts/neighbours, open-service markers |
| Paths (NM4) | traceroute over NM-S3 | `CAP_NET_RAW` | `RemoteHost`/`Router` hop nodes + `Hop` edges |
| Device class / OS (NM-F) | pure inference over the above signals + the OUI database | (none beyond its inputs) | each host's `Classification` → its icon |
| Layer-2 control frames (NM-S5) | passive capture (NM-S4) → sandboxed STP/LLDP/CDP parse | `CAP_NET_RAW` | `Switch`/`AccessPoint` nodes + `Bridged` edges |

## 3. Capabilities

netmap adds **no** new capability. It requests, and the kernel intersects with
the launching user's grants:

`CAP_CONSOLE_WRITE` (fail-loud stderr), `CAP_SHM` (the window frame region and
the discovery delivery/worker pipes), `CAP_FS_ACCESS` (its own `Help/` tree and
app-data store), `CAP_NET` (sockets + discovery), `CAP_NET_DISCOVER_ALL`
(browse every service type and enumerate types — administrative), `CAP_NET_RAW`
(ICMP echo for reachability and traceroute — administrative),
`CAP_SYSINFO_GLOBAL` (interface state, routes, segment-wide reads —
administrative), `CAP_SYSINFO_HW` (interface and neighbour MACs —
administrative), and `CAP_LOG_EMIT` (audit the probe campaigns). Each is
requested in the increment whose code exercises it — `CAP_NET_RAW` and
`CAP_LOG_EMIT` with NM3 — never ahead of it, since an unexercised request is
unaudited surface. Because the tool's core data needs the administrative
ceiling, netmap is an administrator's
tool exactly as `ss` and `ping` are; this is stated in its README and
`docs/src/userland/networking.md`, and it degrades (§2.24) under a narrower
grant rather than failing to launch.

The seam increments add no capability either: `NET_NEIGHBORS` reuses
`CAP_SYSINFO_HW` (it exposes MACs), `NET_ROUTES` reuses `CAP_SYSINFO_GLOBAL`,
and the traceroute path reuses `CAP_NET_RAW` (the `ping` precedent). The §5.2
minimalism test is met by *not* minting anything.

## 4. Increments

### NM1 — the pure engine

`userland/apps/netmap` (`tairix-netmap`), library-only until NM2 adds the
`Run` binary. `no_std` + `alloc`, `#![forbid(unsafe_code)]`:

- `model` — the `Map` graph: `Node`/`NodeKind`, `Edge`/`EdgeKind`,
  content-derived `NodeKey` identities interned once, each node's
  `Classification` (defaulting to `Unknown`, so a node always has an icon),
  and the fixed containment bounds `MAX_NODES`/`MAX_EDGES`/`MAX_LABEL_LEN`;
  an exceeded bound sets `Map::truncated`. Edges deduplicate by
  `(from, to, kind)`; self-loops are refused.
- `ingest` — `Inventory`, the source of truth, and `Inventory::build`, which
  projects it into a fresh `Map` each time it changes rather than mutating one
  in place, so a withdrawn record simply is not projected next time.
- `layout` — the deterministic radial tree: the local host at the origin,
  breadth-first rings `RING_STEP` apart, children ordered by `NodeKey`, wedges
  shared by subtree leaf count; unreachable nodes hang off the root.
- `viewport` — the graph-space `Viewport`: window↔graph mapping, the
  anchor-holding `set_zoom_about` (pinch and Ctrl+wheel), `pan_by`, and
  `clamp_pan` to the laid-out extent plus a margin.

### NM2 — the GUI app

- `source::Snapshot` reads the four System Information sources
  (`NET_INTERFACE_FACTS`, `NET_INTERFACE_STATE`, `NET_NEIGHBORS`,
  `NET_ROUTES`), each whole or refused and held to `MAX_NODES` records; a
  refusal becomes a stated notice, never a failure.
- `view::MapView` is the one input entry point and the renderer: primary drag,
  wheel and two-finger scroll pan (through `lib/controls`' `wheel_steps`);
  `Ctrl`+wheel and pinch zoom about the pointer, a pinch relative to where it
  began and a cancel returning there; a click selects by hit-test and the
  selection follows its node across rebuilds by `NodeKey`. The view fits the
  whole map (`Viewport::fit`, never past 1×) until the user pans or zooms, and
  `Home` fits it again. Nodes are constant-size `lib/icon` round badges — the
  glyph in white on a plate of the node's `BadgeHue` (`node_look`), a service in
  its host's hue — from bounded raster caches, and each edge is drawn in the
  hue of the node it leads to, the default route and named routes heavier.
  Plates and then names are placed into an `Occupancy` index (exact boxes,
  bucketed by cell, every table reused between frames): the selection's name
  first, then the map's own order, each only where it overlaps no plate and no
  earlier name; a spread's names are a pass of their own (NM2d). The selected
  node's facts — its
  guesses with their confidence and every piece of evidence — are a
  `lib/controls` `FactList`.
- `run.rs`: the window over `tairix_window::app`, every read on a
  `tairix_rt::work::Worker` whose wake joins the event wait-set, one present
  per drained burst, and the re-read on a one-shot deadline (`REFRESH_NS`) only
  while a window is open — the neighbour cache and routes have no event to wake
  on. With no worker the read runs inline and the loop collects it without
  parking; discovery and naming, which make many calls, are off without a
  thread of their own (`Collect::start`), and a notice says so.
- The bundle: `AppInfo.toml` (`kind = "application"`, `library =
  "SystemTools"`, `CAP_CONSOLE_WRITE`/`CAP_SHM`/`CAP_FS_ACCESS`/
  `CAP_SYSINFO_GLOBAL`/`CAP_SYSINFO_HW`, and NM2b's `CAP_NET`/
  `CAP_NET_DISCOVER_ALL`, pinned in `program_manifests` as `NETMAP_REQUEST`),
  the 256² `netmap.png` master, the 13-locale `Help/` tree,
  `docs/src/desktop/netmap.md`.
- The window opens titled *Network Map — Reading…* and takes *Network Map*
  only after presenting the frame carrying the first read, so the session's
  `WINDOW_RETITLED` witness means real data is on screen.

### NM2b — advertised services and names

- `discover::Discovery` is the pure engine over an injected `Directory`
  (`Session` implements it): the type list, a browse per type, a resolve per
  instance, a host lookup per `.local` resolve target, and a reverse lookup
  per link-local address the map shows, each kind bounded (`MAX_TYPES`,
  `MAX_SERVICES`, `MAX_HOSTS`, `MAX_REVERSES`), their sum asserted below
  `discovery_ipc::REQUESTS_PER_ACCOUNT`. `reconcile` diffs the open set
  against a walk down from the type list once per collect, so retirement
  unwinds every level at once; a `Lost` request is replaced by starting the
  new one before stopping the old, so the shared question stays asked and is
  replayed from the decoder's cache — except at the account's bound, where the
  old one stops first to make the room. A replacement lost again within its
  wait is held back (`deferred()`) until the wait passes: `REFRESH_NS`,
  doubling with each loss in a row up to `MAX_BACKOFF_DOUBLINGS` (six), and
  forgotten once a replacement outlives it; one still lost at the longest
  wait marks the pass `truncated()`. So one the service cannot replay whole is
  retried on a clock, never as fast as its losses or doorbells arrive. A refusal bears on what it refused: `PermissionDenied` and
  `OutOfRange` drop that ask while it stays wanted (`refused()` reports the
  first, and `locked_out()` says the type list itself was refused), anything
  else — the service's bounds, its absence — ends the pass (`stalled()`), and
  the next pass asks again; `truncated()` is the last pass's. `services()` and
  `names()` are the view.
- Nothing a peer authored reaches the model raw: labels and `TXT` strings
  through `tairix_net::dns::DisplayLabel` (printable Unicode kept, controls and
  default-ignorable characters escaped as `\DDD`), names through `WireName`,
  types through `Presentation`.
- `Inventory` holds the services, the segment's names, and the reverse DNS
  answers (DNS preferred over the segment, either over the address). A service
  hangs under the node at its host's address, a neighbour made for a host only
  discovery knows, or its interface until resolved.
- `run.rs`: a doorbell `PrivatePort` in the event wait-set (counted only for
  a `genuine_doorbell` naming the session `OpenSession` publishes); a queued reader worker for reads and collects, a queued
  namer worker for reverse DNS; collect state `Off`/`Idle`/`Due`/`Asked`/
  `Retry`, a failed collect — or a pass that left work, stalled or deferred —
  retried at the next read and a session refused `PermissionDenied` never
  asked for again, names asked only after a read so a refusing namer costs one
  ask per refresh. A refused type list says discovery is not permitted rather
  than that some services are not shown.

### NM2c — the live QEMU vertical

- `tests/integration/netmap_qemu_aarch64` boots the graphical autoload disk
  (input, framebuffer and virtio-net drivers) — the net-tool disk carries no
  display driver — with `NetPeerMode::V6RouterAdvert`: a link peer emitting an
  autonomous `2001:db8::/64` RA with a non-zero router lifetime, so the guest's
  `net0` autoconfigures a global address and a default route with no
  configuration of its own. The peer ends no run; its verdict needs only an RA
  sent and the guest seen. The RA builder is shared with the DHCPv6 peer.
- The host unlocks, logs in, starts the desktop and clicks `netmap`'s Library
  row. The keyboard gate counts three `irq_bind`s, because the NIC driver binds
  one too. The one screendump is gated on `WINDOW_RETITLED`, the session's word
  that the frame carrying the first read is on screen. *Quit* on the map's slot
  is the last gesture, sent after the dump is read.
- The dump passes when, inside the window's client, at least
  `NETMAP_MIN_TIER_PIXELS` pixels lie on each tier hue's badge ramp (Sky
  interface, Indigo subnet, Orange gateway). Host tests prove one production
  plate clears that count and counts toward no other tier, and that every hue
  the map draws lies more than twice the tolerance from each tier's ramp.
- The guest passes on `APP_LOADED` naming the bundle, then the window's create
  reply, then `netmap`'s own `exit`. A map that dies before drawing data exits
  without a dump, so it cannot pass.

### NM2d — services folded into their host

A printer typically advertises IPP, IPPS, its web console, raw printing and
scanning. Drawn as nodes, those stand five plates on the outer ring, each
taking a wedge from its neighbours. A map of a busy segment then becomes a
crowd of services that hides the hosts. So a service is a fact *about* its
host until the user asks to see it:

- **Folded is the default.** `layout` puts no `Service` node on a ring: each
  sits at its host's position, so a host with twenty services lays out exactly
  as one with none. A folded service is not drawn, has no edge or name drawn,
  and cannot be hit. The model is unchanged: services stay nodes, keep their
  facts and feed the inference. Only their drawing is gated.
- **The host says what it offers.** A host with services wears a count pip on
  its upper shoulder, capped at `9+`; the two lower shoulders already carry the
  liveness light and the OS emblem. Its tooltip — the seat's one line,
  `plans/TOOLTIPS.md` — names the host and its distinct service types in short
  form: `printer.local — 4 services: http, ipp, ipps, uscan`. The line holds
  whole entries only and ends `and N more` at the line's bound. Every other
  node's tooltip is its full name, since the label pass may have had no room
  for it. Tips are declared through `DeclaredTip`, so pointer samples over one
  plate ask once, and none is declared while a drag or pinch is under way.
- **A click spreads, and folds.** A primary click on a host with services
  selects it and spreads its services around it. Clicking the spread host
  again folds them and keeps it selected. Selecting anything else, clicking
  empty space, or pressing `Escape` folds them. A spread service can be
  selected, and selecting it keeps its host spread. At most one host is spread
  at a time: the one folding finishes as the next spreads, and past two the
  older snaps shut.
- **The spread is drawn in window space and is deterministic.** Services stand
  on concentric arcs of fixed on-screen radius around the host, centred on the
  host's outward direction (away from this machine). They fan away from the
  edge that leads in, and stay legible at any zoom. Each arc holds what it can
  carry at one plate pitch. Services are ordered by `NodeKey`, so the same
  services always spread the same way.
- **The animation uses the shared motion machinery.** A spread is a
  `tairix_theme::motion::Fade` of the `PanelOpen` duration, running from the
  strength on screen to full or to none. A fold that interrupts a spread
  therefore reverses from where it stands rather than jumping. Positions follow
  the smoothstep of the strength. The services travel out from under the
  host's plate and are drawn beneath it. Reduced motion is a zero duration: the
  spread lands at once and no frame is owed. Frames are one-shot deadlines
  (`next_frame_in`) folded into the loop's park beside the refresh deadline,
  never a poll, and a settled map arms nothing.
- **A spread is an overlay, so a frame repaints only what it moves.** The map
  beneath is laid out — its names included — as if nothing were spread: a
  spread's lines, plates and names are drawn over it, its names on a backing
  of the canvas colour in an occupancy pass of their own, and the spread host
  on top. A moving service therefore changes nothing outside the reach of its
  spread, and a frame that only moves services presents that reach
  (`frame_damage`) rather than the window.
- **What is drawn is what is hit.** One placement says where every plate
  stands at an instant, and drawing, hit-testing and the tooltip all read it.
  A press finds the topmost plate under the pointer, so a spread service is
  clicked where it is seen, mid-animation included. The detail panel covers
  the map beneath it: nothing under it is hit or tipped.
- **The context menu is the desktop's.** A secondary press asks for the menu of
  the node under the pointer, of the selected node when it lands on that
  node's panel, or of the map itself on empty canvas, anchored at the press (`WindowClient::open_menu`, `plans/NEW-MENUS.md`). The desktop
  places, draws, grabs and answers once with `MenuClosed`. The map acts only on
  the answer naming its own open id. The pure engine builds the `AppMenu`,
  titled with the node's name, and the command each row stands for:
  - *Show Services* or *Hide Services* on a host that has services.
  - On a host, *Copy Address*, or one row per family when it has both (the
    first address of each, a routable IPv6 address preferred).
  - *Copy Name* when the node is named, and *Copy Hardware Address* when its
    MAC is known.
  - On a service, *Copy Address* as `host:port` with an IPv6 host bracketed,
    and *Copy URL* for `_http._tcp` and `_https._tcp`.
  - On a network, *Copy Network*.
  - *Fit Map to Window* on every menu.
- **A URL is built only from parts that are valid in one.** The host is an
  IPv4 or routable IPv6 address, else the advertised host name when it is a
  plain DNS name. The port is written unless it is the scheme's default. The
  `TXT` `path` is used only when every byte of it is a URI path character; a
  peer-authored path that is anything else is dropped, never percent-encoded
  into something the peer did not say.
- **Copying goes to the desktop clipboard** (`tairix_window::clipboard::put`)
  from the focused window. A refused menu or clipboard is reported and the map
  carries on.
- **A node's hardware address is typed.** `Node::mac` holds it and the panel
  renders it, so *Copy Hardware Address* copies the address rather than
  re-parsing a display string.
- **`DeclaredTip` compares the line as well as the region.** A host whose
  services change under a resting pointer then re-declares its tip, and a line
  the wire cannot carry withdraws the last one rather than leaving it standing
  (fail closed).

### NM2e — *Open in Browser*

An `http`/`https` service's menu offers *Open in Browser* once two
prerequisites exist, and this plan builds neither:

- a web browser bundle;
- a desktop URL hand-off: a way for a bundle to declare the URL schemes it
  opens beside the MIME types its `associations` already list, and an open
  request through the desktop's single-instance funnel (`hand_over_launch`)
  that carries a URL rather than a path.

With both in place, the row hands the URL *Copy URL* already builds to the
first installed bundle claiming its scheme, and is absent when none does.
Until then the menu offers *Copy URL* and no row that could not act.

### NM-ICONS — the device-class icon vocabulary

`IconKind::{Computer, Laptop, Server, Router, NetworkSwitch, Phone, Nas,
IotDevice, DeviceUnknown, Subnet}` (asset ids `computer` … `subnet`) and the OS
emblems `IconKind::{OsLinux, OsWindows, OsApple, OsAndroid}` (NM-F), authored
as `lib/icon` symbols — SVG path data on the 24-unit grid through the one
flattener and stroker — so they render at the exact side with no resampling
artefacts and share the Settings symbols' weight and corner radii. Their
built-in pictures are plain tinted glyphs (`plans/ICONS.md` §14); the map
stands each device glyph on a round plate of the node's hue through
`lib/icon`'s one `badge` renderer, and an emblem on a white sticker. An access point
draws the existing `WiFi` symbol, a printer `Printer`, an interface `Ethernet`,
and "beyond this network" `Networking`; a node whose class is unknown draws
`DeviceUnknown`, so no node is ever iconless.

### NM-F — passive device-class and OS inference

- `infer::infer` is pure over `Signals` — the routing role (`Gateway`,
  `Router`), the MAC OUI vendor, and the advertised `AdvertisedService`s — and
  answers a `Classification` (device class and OS family, each with its own
  confidence) and the `Evidence` it rests on. Rules are data: `VENDORS` (the
  registered name's leading words, any case, longest entry speaking; a maker of
  many kinds of thing has no entry), `SERVICES` (DNS-SD types, any case, noted
  in the table's spelling so two spellings are one signal), `MODELS` (the TXT
  `model`/`am`/`md` value's family name, exact case, ending before a non-letter)
  and `osxvers`. Weights for one guess combine as independent hints (noisy-OR in
  integer hundredths of a percent); the winner is reported at its score less
  half the runner-up's; an exact tie is no guess but keeps its evidence; a
  signal-free host is `Unknown`. Every vendor rule must name an organisation the
  committed registry lists, which a test checks.
- `OsFamily` is `Unknown`, `Tairix`, `Linux`, `Android`, `Windows`, `MacOs`,
  `Ios` (iOS and its derivatives) and `NetworkOs` — the families a passive
  signal can tell apart; nothing passive tells a BSD from Linux, so neither
  emits it.
- `Inventory::build` collects each host's role, MAC and served services and
  classifies every `Neighbour`/`Router`/`Gateway`/`RemoteHost` in one last pass;
  a locally administered or group MAC (`assigned_oui` is `None`) names no maker
  and its fact says so.
- The OUI table is `lib/devids`' `DbKind::Oui`: the IEEE MA-L CSV converted by
  `cargo xtask devids --fetch oui.ids` (names respaced and stripped of
  characters that hide or reorder text; assignments listed for several
  organisations, or held by the registry to sub-allocate, omitted — each counted
  in the snapshot's provenance), vetted, compiled to
  `userland/apps/netmap/Resources/oui.ids.bin` and drift-gated in `ci`. The
  reader worker loads it once through `tairix_devids::bundled::load`
  (`CAP_FS_ACCESS`) and keeps the validated `DevIds<Vec<u8>>`; each read names
  the neighbours' makers, display-escaped, or states why it cannot.
- The OS emblems are `lib/icon` symbols — `OsLinux` (a penguin), `OsAndroid` (the
  robot's head), `OsWindows` (a four-paned window), `OsApple` (the looped square
  of the Command key, for macOS and iOS) — drawn on a white sticker on the node's
  shoulder (`os_emblem`); a router's own glyph says what it runs, and this
  machine is labelled as itself, so `NetworkOs` and `Tairix` have none.

### NM-S1 — neighbour-cache read seam

- `lib/net`: `NeighborTable::iter` — an observation like `entry`, refreshing
  no recency, so a read never changes which entry eviction takes. The one
  phase→state mapping is `Entry::observed`, shared by both.
- `lib/abi`: `NetNeighborRecord` (44 bytes; `mac` present exactly when the
  state is not `Incomplete`, enforced at decode) with `NetNeighborState`,
  the paged `NetstackRequest::Neighbors` (op 15), and
  `SysinfoQueryId::NET_NEIGHBORS` (50), gated `CAP_SYSINFO_HW` and audited
  because the entries are other machines' hardware addresses.
- `netstack`: `Netstack::neighbor_records`, served behind the broker's
  `CAP_SYSINFO_INTROSPECT` gate; `sysinfod` pages it as `net_neighbors`.
- `lib/procinfo`: `for_each_net_neighbor` (`nettopo`), `render_neighbor`,
  and the sensitive `state:net/<iface>/neighbors` reading.
- Engine: a neighbour becomes a `Neighbour` node under the narrowest subnet of
  its interface that contains it (else the interface), with its MAC; a
  neighbour that is a router or the gateway joins that node instead.

### NM-S2 — routing-table + gateway read seam

- `lib/net`: `Stack::{routes_v4, routes_v6, default_routers, neighbors}`
  read-only views and `DefaultRouterList::iter`.
- `lib/abi`: `NetRouteRecord` (52 bytes) with `NetRouteOrigin`
  (`Connected`/`Static`/`RouterAdvertisement`/`Redirect`/`DefaultRouter`; a
  `DefaultRouter` record must be the default prefix through a next hop), the
  paged `NetstackRequest::Routes` (op 16), and `SysinfoQueryId::NET_ROUTES`
  (51), gated `CAP_SYSINFO_GLOBAL` and audited. One network-prefix definition,
  `ipv4_network`/`ipv6_network`/`ip_network`, which `lib/net` now uses too.
- `netstack`: `Netstack::route_records` lists each interface's IPv4 and IPv6
  routes and the default route through every RA-named router, so a DHCP- or
  RA-learned gateway is visible. A DHCP gateway reports `Static`, as the
  engine installs it.
- `lib/procinfo`: `for_each_net_route`, `render_route`, and
  `state:net/<iface>/routes`, keeping `plans/NETWORK.md` §5's promise. A
  multi-valued reading too long for `MAX_INFO_VALUE_LEN` keeps whole entries
  and ends `, and N more` rather than failing.
- Engine: default routes are projected first, so their next hop is one
  `Gateway` node (router class) leading to the `Beyond` sink; a named route's
  next hop is a `Router` with the network behind it.

### NM3 — active reachability probing

The pure probe engine: a token-bucket rate limiter and a bounded-concurrency
campaign scheduler over an injected echo/stream-socket + clock seam, emitting
one-shot deadlines (never a spin, §2.23). The rate limit is `lib/net`'s one
`rate::TokenBucket`, whose `ready_at` query gives a tickless caller the
next-token instant to arm its timer. The ICMP-echo subnet sweep (bounded
to attached subnets, unicast only) and the TCP-connect check against a
curated, data-defined well-known-service port set (connect-only; a
refusal/timeout is a datum, not a retry). The stack refuses only the limited
broadcast and multicast destinations, so the sweep itself skips each attached
subnet's network and directed-broadcast addresses. The stack has no
connect-with-deadline and an unanswered SYN takes minutes to fail, so the
engine imposes its own connect deadline; a refusal arrives as
`Closed { Refused }`. The reply-TTL evidence reads the echo reply's hop limit,
carried on `StackEvent::EchoReply` and in the `SocketEcho` frame's spare byte
(landed; `ping` prints it as iputils' `ttl=`). Liveness and open-service markers flow onto the map's existing
host/neighbour nodes. The settings pane (`passive`/`polite`/`thorough`, rate,
concurrency) persisted via `tairix_appdata` and read at start (§28 — never
written from the loop). Every campaign start/stop and refusal audited via
`CAP_LOG_EMIT` with stable event ids. Fuzz the probe-engine input handling.

### NM-S3 — the traceroute seam + `traceroute` command app

- `lib/net`: `Stack::send_echo_request` takes an `EchoProbe` whose optional
  `hop_limit` becomes the IPv4 TTL or IPv6 hop limit. `IcmpError::quoted_echo`
  reads the quoted packet's IP header and the echo identifier and sequence —
  without the quoted header's checksum, which a router quotes as it held it —
  and the stack raises `StackEvent::EchoError { reporter, destination,
  identifier, sequence, kind }` instead of `IcmpErrorReceived` only when the
  quoted source is this interface's own address. The identifier alone never
  attributes an error. `trace::PathTrace` is the one hop-by-hop engine: which
  probe to send at what hop limit, a bounded window of probes in flight, each
  answer's meaning, and the order hops settle in; NM4 drives it too.
- `lib/abi`: `SocketRequest::SendEcho` carries `hop_limit` in byte 12 (zero is
  the default), and `SocketEcho` carries an `EchoOutcome` (`Reply`,
  `HopLimitExceeded`, `Unreachable { code }`, `PacketTooBig { mtu }`,
  `ParameterProblem { code, pointer }`), so a probe socket reads one ordered
  stream; an error carries no payload on either side of the wire.
- `netstack`: one `deliver_echo` path for replies and errors. An error reaches
  the echo socket with its CSPRNG-drawn identifier, filtered for a connected
  socket on the quoted *destination* (the error comes from a router, not the
  peer); a reassembly timeout answers no probe. Gate stays `CAP_NET_RAW`.
- `lib/rt::net::EchoSocket` is the one connected echo socket `ping`,
  `traceroute` and the map probe through: its own delivery port and wait-sets,
  released when it drops, with answers and network changes waited on
  separately so neither wait can be cut short by the other's member.
- `userland/apps/traceroute`: the Linux `traceroute` surface for ICMP echo —
  `-46In`, `-f`/`-m`/`-q`/`-N`/`-w`, `packetlen` — refusing UDP/TCP methods by
  name; hops print in order once settled, with the family's `!` annotations.
  `ping` prints iputils' `From <addr> icmp_seq=<n> <reason>` line for an error
  outcome and counts it in its statistics.
- The `netstack_icmp_qemu_aarch64` vertical runs both tools against the echo
  peer, which plays two routers in front of itself, and requires each hop.

### NM4 — inter-network topology

Engine hop-by-hop path discovery to a user-entered target (LAN or WAN,
operator-directed) over NM-S3, with per-hop reverse DNS and a bounded hop
count; the discovered routers and remote host extend the map outward from the
gateway through `Hop` edges. This is the "clever exploration of less-local
networks" — principled, operator-directed path tracing, not uninvited probing.

### NM-S4 — the passive packet-capture seam

The receive-only link-layer capture facility, built defensively. `lib/abi`: a
capture socket kind (or a capture mode on a raw socket) and a `CapturedFrame`
delivery frame carrying the raw bytes plus the interface and a timestamp,
bounded by a fixed per-frame cap; a bounded capture ring that drops oldest
under pressure (never grows, §24.4). netstack: a per-interface **capture tap**
placed *upstream* of the receive pre-filter (`lib/net::rxfilter`, which today
sheds every non-IP ethertype), active only while a `CAP_NET_RAW`-holding
capture socket is bound to that interface, so STP/LLDP/CDP/etc. reach the
subscriber without altering the stack's own data path. The tap **never injects**
and exposes no transmit path. `lib/rt::net`: `capture_socket` / `recv_frame`
wrappers. Enabling, disabling, and dropping-under-pressure are audited with
stable event ids (§19.4). The driver seam already delivers whole frames to the
stack; the tap copies them before the shed, so no driver change is needed
beyond confirming the pre-shed delivery point. The consumer is NM-S5; the seam
lands with it (§2.4). Reuses `CAP_NET_RAW` — no new capability.

### NM-S5 — layer-2 topology from capture

The switch-diagramming path, best-effort. New pure `lib/net` parsers — `stp`
(802.1D/802.1w BPDUs: bridge id, root id, port id, path cost), `lldp`
(802.1AB TLVs: chassis/port id, system name/description, management address,
port VLAN), and `cdp` (the Cisco Discovery vocabulary) — each total, bounded
(§24.4), fuzzed (§19.6), and fail-closed. A captured frame is **hostile input
from an unauthenticated peer**, so it is parsed only inside a §19.5
minimum-capability sandbox: a new `lib/sandbox` capture-decode worker (the
`timesync`/`imagerender` precedent) holds the capture socket and the parsers and
nothing else; the front holds `CAP_NET_RAW` and the map. The engine turns LLDP
`Switch`/`AccessPoint` identities and their port adjacencies into `Switch`/
`AccessPoint` nodes and `Bridged` edges, and STP root/bridge ids into the
spanning-tree shape (root switch, this host's upstream bridge). Where nothing is
seen, nothing is drawn (§2.24). Every byte displayed is escaped (§5).

### NM5 — UX completion

The node detail panel (`FactList`), search/filter over labels and addresses,
service-type badges, liveness/freshness shading, keyboard navigation, the map
legend, and the complete settings pane. The README currency review (§13) and
the `docs/src/` page are brought final.

## 5. Tests, docs, and gate (binding)

Every increment lands its unit/property/fuzz/QEMU tests in the same change
(§7), registers any fuzz harness with `cargo xtask fuzz`, updates its rustdoc +
`docs/src/` page + this plan's ledger (status only, no landing narrative, §13),
and ends with the full §2.15 gate: `cargo fmt --all`, `cargo xtask ci` (once),
`cargo xtask fuzz --secs 5`, and `tools/ci/soak.sh both --secs 20`, quoted in
the completion report. A seam increment that touches `unsafe` or a
synchronisation primitive runs the §19.11 oracles and says which looked.

## 6. What this plan explicitly does *not* do

- No stealth/evasion scanning, no half-open/SYN scan, no malformed-packet or
  active-probe OS fingerprinting (nmap `-O`), no exploitation — not
  first-class-OS features, and against the charter's posture (§5, §19). OS/
  device inference is *passive only* (NM-F).
- No frame injection, spoofing, or transmit through the capture seam — capture
  is strictly receive-only (NM-S4). No ARP/ND spoofing, no rogue RA/DHCP, no
  man-in-the-middle tooling of any kind.
- No uninvited probing of networks the host is not attached to; sweeps are
  bounded to attached subnets, and only operator-entered single targets reach
  beyond them.
- No LLMNR, no NetBIOS, no off-link mDNS, no amplification (ZEROCONF §1, §8).
- No service publication (ZEROCONF Z5), no `files.app` network catalogue /
  share mount (ZEROCONF Z8/Z9), no firewall/NAT/forwarding (NETWORK §9).
- No SNMP or SSDP inventory and no LLDP transmission in this plan — listening
  to neighbours' LLDP is NM-S5; the rest each need their own stack surface and
  are a later plan (§2.3).
