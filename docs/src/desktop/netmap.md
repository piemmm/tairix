# Network Map

`netmap.app` (`userland/apps/netmap`, `plans/NETMAP.md`) is a pannable,
zoomable map of the networks this machine is on: its interfaces, the subnets
they are on, the hosts its neighbour cache has seen, the routers its routes go
through, the default gateway that leads beyond the local network, and the
services the hosts on the local link advertise.

## What it reads

Every source is capability-checked, and none is a probe of a host:

| Source | Through | Capability |
|---|---|---|
| Interface facts (MAC, MTU, kind) | `NET_INTERFACE_FACTS` | `CAP_SYSINFO_HW` |
| Interface state (link, addresses) | `NET_INTERFACE_STATE` | `CAP_SYSINFO_GLOBAL` |
| Neighbour cache (ARP and ND) | `NET_NEIGHBORS` | `CAP_SYSINFO_HW` |
| Routes and IPv6 default routers | `NET_ROUTES` | `CAP_SYSINFO_GLOBAL` |
| Advertised services and link-local names | the discovery service (discovery-v1) | `CAP_NET_DISCOVER_ALL` |
| Host names | reverse DNS through `lib/resolver` | `CAP_NET` |
| Hardware makers | the bundle's IEEE MA-L table, `Resources/oui.ids.bin` (`lib/devids`) | `CAP_FS_ACCESS` |

Each source is read whole or refused, and each read is held to the map's node
bound, so a segment of hostile size cannot grow it without limit. A refused
source is an answer, not a failure: the window states what it cannot show and
why, and draws the rest. Every read runs on a worker thread, so the window
answers within a frame whatever the stack is doing.

The neighbour cache and the routes change with no event to wake on, so while a
window is open the map is read again on a one-shot deadline five seconds after
the last read landed. With no window open nothing is read and no timer is
armed.

Until the first read lands the window is titled *Network Map — Reading…*, so a
window opened on an empty map says it is still gathering rather than reading as
blank. It takes the title *Network Map* only after presenting the frame that
carries that read: the session serves a window's requests in order, so the new
title reaches the screen with the data, never ahead of it.

## Services and names

The map keeps one discovery session open, ringing a doorbell port that is a
member of the window's own wait-set, so it collects only when the service says
its answers moved. The pure `Discovery` engine enumerates the segment's service
types, browses each, resolves each instance, looks up each host under
`.local` an instance resolves to — a unicast name is not the link's to answer
— and asks for the names of the link-local addresses on the map, every kind
bounded (`MAX_TYPES`, `MAX_SERVICES`, `MAX_HOSTS`, `MAX_REVERSES`), together
well inside what the service grants one account. It reconciles the open
requests against what is held once per collect, walking down from the type
list, so a type that goes takes every browse, resolve and lookup below it with
it, and a request whose updates the service dropped is asked again — started
before the old one stops, so the service replays what it holds rather than
asking the segment again, unless the map is at its account's bound, when the
old one stops first to make the room. One lost again as soon as it was asked
again waits for the next read, so a request too large to replay whole costs a
retry a read rather than a storm.

A refusal bears on what it refused, and the map says which. An ask the map
may not make (`CAP_NET_DISCOVER_ALL` withheld) or the service will not
answer is not made again while it is wanted, so it is refused and audited
once, and every other ask goes on; a service at its bounds ends that pass, and
the next read asks again. When the type list itself is refused the map says
service discovery is not permitted. A session the service refuses outright is
not asked for again. Every label a
peer authored is shown as a `DisplayLabel`:
printable Unicode as DNS-SD names are written, but every control and every
character that formats, joins or hides text escaped, so no label can pass for
another by an invisible difference.

A service belongs to the node at one of its host's addresses — a neighbour, a
router, or this machine — to a neighbour made for it when only discovery knows
the host, and to its interface until the host's addresses are known. It is
folded into that node until the user spreads it (see *Services*, below). A host is labelled with its reverse DNS name, else with what the
segment calls it, else with its address; both names are kept as facts. Reverse
DNS runs on a worker of its own, one lookup at a time, so a slow resolver never
holds up a refresh; each routable address is looked up once per run. A
discovery session that fails, or a DNS transport refused, is stated on the map
and asked again at the next read.

## What each host is

Every host is classified from what the map already holds — nothing is probed
for it. `infer` reads four kinds of signal, each rule suggesting a device
class, an OS family, or both, with a weight out of 100:

- the routing role it plays (the default gateway, a named route's next hop);
- the organisation its hardware address was assigned to, matched on the
  leading words of the registered name, so `Cisco Meraki` and `Cisco Systems`
  can say different things; a locally administered address (a privacy or
  virtual-machine address) names no maker and says so;
- the DNS-SD service types it advertises (`_ipp._tcp`, `_companion-link._tcp`,
  `_googlecast._tcp`, …);
- the model its TXT data names (`model=`, `am=`, `md=`, `MacBookPro18,3`,
  `AppleTV11,1`, `Chromecast`) and a stated macOS version (`osxvers=`).

Weights for one guess combine as independent hints, each removing its share of
the doubt left; the strongest guess wins less half the runner-up's score, so
conflicting evidence lowers the confidence; an exact tie is no guess. A host no
rule speaks for stays unknown. The detail panel states the guess with its
confidence ("Laptop (95% sure)", "Runs macOS (99% sure)") and every piece of
evidence behind it. The organisation table is the IEEE registry as `lib/devids`
vets and compiles it; a block the registry holds to sub-allocate, or one listed
for several organisations, names no maker rather than a wrong one.

## The map

`Inventory::build` projects what was read into a fresh `Map` each time, so a
withdrawn record simply is not drawn next time. One device is one node: a
router the routing table names and the neighbour cache resolves is drawn once,
with its hardware address. Default routes are projected first, so a next hop
that carries both the default route and named routes is drawn as the gateway.
The layout is a deterministic radial tree around this machine, so the same
network always draws the same way. Services take no place on it: each is
folded into its host, so a host advertising a dozen services lays out as one
offering none.

Nodes are drawn at a constant size whatever the zoom, each as its glyph in
white on a round plate of the hue of what it is, the badge hues the desktop's
Settings categories use (`lib/icon`'s `BadgeHue`): computers blue, servers
graphite, routers and switches orange, interfaces and wireless sky, networks
indigo, printers purple, storage teal, phones green, connected devices pink,
and what nothing has identified grey. A service wears its host's hue, and every
line wears the hue of the node it leads to, so the map reads by colour before
its names do; the default route and named routes are drawn heavier. A host
no inference has classified draws `DeviceUnknown`, never nothing. One shoulder
carries a light saying whether the node was last seen reachable; the other
carries its system's emblem on a white sticker when one was inferred — a
penguin for Linux, the Android robot's head, a four-paned window for Windows,
the looped square for Apple's systems.

Every name that has room is drawn, the selection's first and then in the map's
own order (this machine, its interfaces, networks, routers, hosts):
plates and names are placed into an index of the canvas and a name that would
overlap either is left out, so a zoomed-out map is never a smear of text and a
sparse one is named at any zoom.

The view keeps the whole map in sight as it changes — fitted with a margin,
never magnified past its own size — until it is panned or zoomed; from then on a
refresh leaves it where it was put, and `Home` fits it again.

## Services

A host that advertises services wears their count on a pill at its upper
shoulder (`9+` past nine), and its tooltip names it and the services it
offers by type — `printer.local — 4 services: http (2), ipp, uscan` — whole
entries only, ending `and N more` at the tooltip's one-line bound. Every
other node's tooltip is its full name, which the label pass may have had no
room for. The map declares the tip for the plate under the pointer through
the seat's one tooltip (`plans/TOOLTIPS.md`), asking again only when the plate
or its line changes, and declares none while a drag, a pinch or a spread is
moving.

Clicking a host that offers services spreads them around it: each stands on an
arc of fixed on-screen size around the host, fanned away from this machine so
they clear the line that leads in, nearer arcs filled first, ordered by
identity so the same services always spread the same way. They travel out from
under the host's plate over the theme's panel-opening time, smoothed at both
ends; with reduced motion they land at once. Clicking the spread host again
folds them, and so do selecting anything else, clicking empty space and
`Escape`. A fold that interrupts a spread reverses from where it stands. A
spread service can be selected like any node, and keeps its host spread. Each
frame of the movement is a one-shot deadline folded into the window's park
beside the refresh, never a poll; a map at rest arms nothing.

A spread is drawn over the map, which is laid out — names included — as if
nothing were spread: the lines to the services, their plates, and their names
on a backing of the canvas colour, then the host on top. So a moving service
changes nothing beyond its spread's reach, and a frame that only moves
services repaints and presents that reach rather than the window.

One placement says where every plate stands at a given instant, and drawing,
hit-testing and the tooltip all read it, so a plate is clicked where it is
drawn — the topmost one under the pointer — including mid-movement. The detail
panel covers the map beneath it: nothing under it is clicked or tipped.

## The context menu

A secondary press opens the menu of the node under it, of the selected node
when it lands on the detail panel, or of the map on empty canvas. The map
builds the model and the desktop places, draws, grabs and answers it once
(`plans/NEW-MENUS.md`); the map acts only on the answer naming its own open.

| Row | Offered on | Does |
|---|---|---|
| *Show Services* / *Hide Services* | a host offering services | spreads or folds them |
| *Copy Address* | a host known by one family of address; a service | the address; for a service `host:port`, an IPv6 host in brackets |
| *Copy IPv4 Address*, *Copy IPv6 Address* | a host known by both families | the first of each, a routable IPv6 address before a link-local one |
| *Copy URL* | a `_http._tcp` or `_https._tcp` service | its URL |
| *Copy Name* | a node named other than by its address | its name |
| *Copy Hardware Address* | a node whose MAC is known | its MAC |
| *Copy Network* | a network | its prefix |
| *Fit Map to Window* | every menu | as `Home` |

A URL is built only from parts valid in one, since every part of it was
authored by an unauthenticated peer: the host is the service's IPv4 address,
else a routable IPv6 address in brackets, else its advertised name when that
is a plain DNS name (a link-local address alone would need a zone the URL
cannot carry portably); the port is written unless it is the scheme's own; and
the `TXT` record's `path` is used only when it is an absolute URI path as it
stands — anything else is dropped, never encoded into something the peer did
not say. Copying puts the text on the desktop clipboard from the map's window;
a refused menu or copy is reported on the standard error stream and the map
carries on. *Open in Browser* waits on a web browser and a desktop URL hand-off
(`plans/NETMAP.md` NM2e).

## Input

| Gesture | Effect |
|---|---|
| Primary drag | Pan |
| Wheel, two-finger touchpad scroll | Pan, both axes |
| `Ctrl` + wheel | Zoom about the pointer |
| Pinch | Zoom about the pinch, relative to where it began; a cancelled pinch returns there |
| `Home` | Fit the whole map, and keep it fitted as it changes |
| Click a node | Show its details |
| Click a host offering services | Show its details and spread its services; again folds them |
| Click empty space, `Escape` | Put the details away and fold any spread |
| Secondary click | The context menu |

A burst of input is drained and drawn once.

## On a running machine

The `netmap_qemu_aarch64` vertical boots the graphical desktop with a NIC whose
link peer advertises itself as a router (an autonomous `2001:db8::/64` prefix
and a non-zero router lifetime), so the guest's interface autoconfigures a
global address and a default route with no configuration of its own. It opens
`netmap` from the program library, photographs the window once the session
says it wears the title it takes on its first read, and asserts the map draws
its interface, subnet and gateway tiers: pixels on each tier hue's own badge
ramp, inside the window's client. It passes only once `netmap` exits through
*Quit* on its icon-bar slot, the gesture sent after that photograph is read
back.
