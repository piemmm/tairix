## NAME

traceroute — print the route echo probes take to a network host

## SYNOPSIS

`traceroute [option...] host [packetlen]`

## DESCRIPTION

Sends ICMP (IPv4) or ICMPv6 (IPv6) echo requests towards a host with a hop
limit that climbs one hop at a time, and prints each router that reports a
probe's hop limit running out, until the host itself answers. The probes
flow through an ICMP echo socket opened from the user-space network stack,
gated on `CAP_NET` and `CAP_NET_RAW` and audited.

The target is an IPv4 or IPv6 address literal or a host name, resolved
through the system stub resolver; a literal needs no query.

Each hop prints on its own line once all of its probes have settled: its
number, then for each probe the round-trip time, preceded by the address —
and, unless `-n` is given, the name — of whoever answered whenever it
differs from the probe before. A probe that draws no answer within the wait
prints `*`. A router that reports the path cannot go on marks its probe —
`!N` network unreachable, `!H` host unreachable, `!P` protocol unreachable,
`!X` administratively prohibited, `!S` source route failed, `!F-mtu` packet
too big, `!<code>` any other — and the trace stops at that hop. It also
stops once the host answers, or at the hop bound.

Several probes are outstanding at once (`-N`), so a run of silent hops
costs one wait rather than one per probe; a hop is still printed only once
every hop before it has been.

The probes are ICMP echo requests, the method `-I` selects; an option
selecting UDP or TCP probes is refused rather than quietly ignored.

## OPTIONS

- `-4` — require an IPv4 target.
- `-6` — require an IPv6 target.
- `-I, --icmp` — probe with ICMP echo (the only method, and the default).
- `-f, --first` — the hop limit the first probes carry (default 1).
- `-m, --max-hops` — the highest hop limit probed (default 30).
- `-q, --queries` — probes sent per hop (default 3, at most 10).
- `-N, --sim-queries` — probes outstanding at once (default 16, at most 64).
- `-w, --wait` — seconds to wait for each probe's answer (a decimal,
  default 5).
- `-n` — numeric output: addresses only, with no reverse lookup.
- `-?, --help` — show this command's own short help.

`packetlen` is each probe's total length in bytes, headers included
(default 60 for IPv4 and 80 for IPv6).

## EXAMPLES

- `traceroute example.com` — trace the route to a host.
- `traceroute -n 192.0.2.1` — numeric output, no name lookups.
- `traceroute -q 1 -w 1 -m 20 gateway` — one quick probe per hop, at most
  twenty hops.
- `traceroute -6 host.example` — trace over IPv6.

## EXIT STATUS

- `0` — the trace ran, whether or not it reached the host (or the short
  help was written).
- `2` — the command line was not understood, the target did not resolve,
  the socket could not be opened, or a probe could not be sent.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such as
  `fr-FR`).

## SEE ALSO

- `ping`
- `host`
- `ss`
- `man`
