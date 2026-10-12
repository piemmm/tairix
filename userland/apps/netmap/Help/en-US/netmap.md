## NAME

netmap — network map and inventory

## SYNOPSIS

`netmap`

## DESCRIPTION

Opens a window that maps the networks this machine is attached to: its network
interfaces, the subnets they are on, the hosts its neighbour cache has seen on
each, the routers its routes go through, and the default gateway that leads
beyond the local network. Each is drawn as an icon on a disc coloured for what
it is — computers blue, routers orange, printers purple, storage teal, phones
green — and each line takes the colour of what it leads to. A host whose
operating system the map recognises wears its emblem, and a light beside a
host or interface shows whether it was last seen reachable (green) or not
(red).

Drag with the primary button, turn the wheel, or scroll with two fingers on a
touchpad to move around the map. Hold `Ctrl` and turn the wheel, or pinch on a
touchpad or touchscreen, to zoom about the point under the pointer; press
`Home` to fit the whole map in the window again. Click a node to see what is
known about it — its addresses, hardware address and maker, reachability, and
what kind of device it appears to be and what it runs, with how sure the map
is and why — and click empty space to put the details away. Names appear
wherever there is room for them.

A host that advertises services wears how many on a small badge. Rest the
pointer on any node to see its full name, and on such a host to see its
services named; click the host to spread its services around it, and click it
again, click elsewhere, or press `Escape` to fold them away. Click a node with
the secondary button for its menu: show or hide its services, or copy its
address, name, or hardware address — for a service, its address and port, and
for a web service, its URL.

The map is read through the System Information API and the discovery service,
and the hosts on it are named by the configured DNS servers or by what the
devices call themselves; it never contacts a host itself, and it is read again
every few seconds while its window is open. It also gathers the services
devices advertise on the local link — printers, file shares, media players —
and keeps each with the host that offers it. Interface addresses and routes
need `CAP_SYSINFO_GLOBAL`; hardware addresses and the neighbour cache need
`CAP_SYSINFO_HW`; advertised services need `CAP_NET_DISCOVER_ALL`; naming
hosts by DNS needs `CAP_NET`. Without one, the map says what it cannot show
and draws the rest.

Netmap is launched from the desktop's Program Library or by name from a shell,
and needs a running graphical session; without one it reports the refusal on
the standard error stream and exits.

## EXIT STATUS

Zero after Quit; non-zero when the window channel or the shared frame region
was refused (the reason is stated on the standard error stream).
