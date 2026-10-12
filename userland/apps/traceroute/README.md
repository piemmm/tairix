# tairix-traceroute

TAIRiX `traceroute` — print the route echo probes take to a network host, in
the familiar Linux `traceroute` shape (`plans/NETMAP.md` NM-S3, a
`plans/APPS.md` command app).

The probes are ICMP (IPv4) or `ICMPv6` (IPv6) echo requests whose hop limit
climbs one hop at a time. Each router whose hop limit runs out reports back,
and the trace ends where the host answers, where a router reports that the
path goes no further, or at the hop bound. `-f`/`-m` bound the hop limits,
`-q` sets the probes per hop, `-N` the probes outstanding at once, `-w` the
wait for each, `-4`/`-6` force the family, `-n` keeps the output numeric,
and `-?`/`--help` render the tool's own short help. ICMP echo is the one
method: `-I` names it, and an option selecting UDP or TCP probes is refused
rather than quietly ignored.

## Output

Each hop prints once all of its probes have settled: its number, then each
probe's round trip, preceded by the address — and, without `-n`, the name —
of whoever answered whenever it differs from the probe before; a probe that
drew nothing prints `*`. A router reporting the path cannot go on marks its
probe in the family's own vocabulary (`!N`, `!H`, `!P`, `!X`, `!S`, `!V`,
`!C`, `!F-mtu`, `!<code>`), as Linux `traceroute` does. A hop is printed only
once every hop before it has been, so output never reorders.

## How it reaches the network

The hop-by-hop method is `lib/net`'s shared `trace` engine
(`PathTrace`), which the network map drives too: it decides which probe to
send, when, and what each answer means, and this crate prints it. The probes
go through `tairix_rt::net::EchoSocket`, an ICMP echo socket connected to the
target through the `netsock-v1` ABI served by `userland/net/netstack`, gated
on `CAP_NET` + `CAP_NET_RAW` and audited; the stack owns the echo identifier
and reports each router's Time Exceeded or Unreachable about one of this
socket's requests back to it. The tool never crafts a raw packet and holds no
ambient authority.

Several probes are outstanding at once, so a run of silent hops costs one
wait rather than one per probe. A probe no interface can carry yet waits for
the stack's network notice rather than retrying on a timer, and every wait
parks until its own deadline.

The target and every hop name resolve through `lib/resolver`: the target
through the shared `target_address` policy, so `traceroute`, `ping` and
`telnet` cannot disagree about what a host operand means, and each hop's name
through one transport reused for the whole run, which remembers the names it
has looked up.

## Structure

* `command` — the option grammar (`Command`/`Config`) and its parser.
* `net` — the `TraceIo` seam and the `Answer` it reports.
* `client` — the `run` entry point and the hop printer.
* `error` — `TraceError`, what ends a run early.
* `run.rs` — the freestanding `Run` binary (pure-Rust, `tairix-rt`), which
  implements `TraceIo` over `lib/resolver` and the echo socket.

The bundle's `Help/` documents are authored on disk and read at runtime
through the injected `tairix_help::HelpSource`; help is never embedded in
the program (`plans/APPS.md`).

## Stability

Experimental. The echo-socket path tracks the unfrozen `abi-v1` `netsock-v1`
contract and may evolve until the first release.
