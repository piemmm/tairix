# tairix-netmap

The network map and inventory, `netmap.app` (`plans/NETMAP.md`,
`docs/src/desktop/netmap.md`). Stability tier: **experimental**.

The package is the host-tested map engine and the bundle's `Run` binary. The
binary is thin glue over `tairix-rt`: it reads the network and keeps discovery
current on workers and draws what the engine produces, so the tested code and
the shipped code are the same code.

## What it provides

- `model` — the bounded, idempotent map graph: `Map`, `Node`/`NodeKind`,
  `Edge`/`EdgeKind`, content-derived `NodeKey` identities, and each node's
  `Classification` (device class and OS family, each with its confidence) with
  the `Evidence` it rests on. Every count and length is a fixed containment
  bound; an exceeded bound sets `Map::truncated`, which the window states.
- `source` — `Snapshot`, one refresh's reads through the System Information
  API, each read whole or refused (a refusal becomes a stated notice), and the
  organisation each neighbour's hardware address names, from the bundled OUI
  table.
- `discover` — the pure `Discovery` engine over discovery-v1: service types,
  browses, resolves, host lookups and link-local reverse names, each bounded.
- `infer` — passive device-class and OS inference from the routing role, the
  hardware maker, the advertised service types and the TXT model.
- `ingest` — `Inventory`, the source of truth, and `Inventory::build`, which
  projects interfaces, subnets, neighbours, routers, the gateway and services
  into a fresh `Map`, one node per device, and classifies every host.
- `layout` — the deterministic radial tree around this machine.
- `viewport` — the graph-space `Viewport`: fit, anchor-holding zoom, clamped
  pan, hit-testing.
- `view` — `MapView`: drag, wheel and two-finger scroll pan, `Ctrl`+wheel and
  pinch zoom, `Home` to fit, selection, and the renderer: hued badges, OS
  emblems, lines in the hue they lead to, and names placed where they have
  room.

## Capabilities

`CAP_CONSOLE_WRITE`, `CAP_SHM`, `CAP_FS_ACCESS` (the bundled OUI table),
`CAP_SYSINFO_GLOBAL` (interface state, routes), `CAP_SYSINFO_HW` (hardware
addresses, the neighbour cache), `CAP_NET` (reverse DNS) and
`CAP_NET_DISCOVER_ALL` (every advertised service type). The `SYSINFO` pair and
`CAP_NET_DISCOVER_ALL` are administrative; under a narrower ceiling the map
draws what it may read and states the rest.

## Security

Everything drawn was read from capability-checked system services or the
bundle's own vetted table. Text a peer authored — service instance names, TXT
strings, host names — is escaped before it reaches the model, so nothing
invisible or reordering can make one name pass for another. An inferred
classification is never asserted as fact: the panel shows its confidence and
its evidence.

## Tests

Host tests cover interning and the containment bounds; the projection of
interfaces, subnets, neighbours, routers, the gateway and services; one node
per device; the discovery walk and its bounds; every inference rule table, its
matching boundaries, the combination of evidence and ties, and that every
maker rule names an organisation the committed registry lists; maker naming
and its notice; the layout's determinism; the viewport's round trip, fit,
anchor-holding zoom, pan clamping and hit-testing; every gesture and the fit
that follows the map until steered; the selection across a rebuild; the glyphs,
hues and emblems; label placement; refused sources becoming notices; and
drawing.

The `netmap_qemu_aarch64` vertical opens the map from the program library on a
booted desktop whose NIC has autoconfigured against a router-advertising peer,
and asserts its interface, subnet and gateway tiers are drawn.
