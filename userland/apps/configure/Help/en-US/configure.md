## NAME

configure — read and set the machine's settings

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

Lists, shows, and sets the machine's settings, kept at
`/System/Settings/Configuration/system.conf`. With no operand every
setting is listed with its current value; with a key alone that
setting's value is shown; with one or more `<key> <value>` pairs those
settings are changed together.

Several pairs are one change, not several: every pair is checked before
anything is changed, and the settings are then saved at once. A command
line naming an unknown key, a value outside its key's set, or the same
key twice changes nothing at all, so a group of settings can never be
left half applied.

Changing a setting needs an administrator: an ordinary account reads the
machine settings but changes none. The system ships settings of its own,
which stand until an administrator changes one, and the first change
keeps every other setting the system shipped. A change takes effect the
next time the part of the system that reads it starts — `os.loginType`
at the next login, the `cache.*` switches at the next boot — unless its
key below says otherwise.

The set of keys is closed: an unknown key, or a value outside a key's
set, is refused with the valid choices stated and changes nothing.

- `os.loginType` — `text` or `graphical`: which session type the login
  service starts for an authenticated user. `graphical` (the default)
  starts the desktop session directly after authentication, degrading
  to the text login on a machine that cannot run one; `text` starts the
  account's shell — the desktop runs only from a graphical login.
- `cache.all` — `on` or `off`: the master caching switch. `on` (the
  default) lets each cache class below follow its own setting; `off`
  is a ceiling that disables every memory cache regardless of the
  per-class settings.
- `cache.filesystem`, `cache.block`, `cache.transform`,
  `cache.semantic` — `auto` or `off`: the per-class switches for the
  four reclaimable memory caches (the filesystem, whole-disk block,
  decompressed-cluster, and application-launch caches). `auto` (the
  default) lets the memory-pressure manager govern the class; `off`
  disables it outright. There is no per-class `on`: a class cannot be
  forced to ignore memory pressure. A class is effectively `off`
  whenever `cache.all` is `off`.

Every cache is a reclaimable accelerator, never the source of truth, so
turning any or all of them off only makes the affected work slower — it
never changes a result.

- `net.ipv4.enabled`, `net.ipv6.enabled` — `true` or `false`: the
  stack-wide address-family switches. Both are `true` by default. A
  disabled family binds no addresses, answers no packets, and refuses
  a socket in that family with a typed error — never a silent drop.
- `net.ipv6.privacy` — `true` or `false`: whether the stack forms
  temporary (privacy) IPv6 addresses alongside the stable one. `false`
  (the default) uses the stable SLAAC address only.
- `net.tcp.syncookies` — `auto` or `always`: the SYN-flood defence
  policy. `auto` (the default) keeps a bounded half-open queue and
  falls back to stateless cookies on overflow; `always` answers every
  connection request statelessly. There is no `off` — an undefended
  connection queue is never a setting.
- `net.tcp.keepalive` — `true` or `false`: whether TCP connections
  send keepalive probes on an idle link. `false` (the default) never
  probes and never tears an idle connection down for inactivity;
  `true` probes an idle peer after the standard idle interval and
  drops the connection if the peer stops answering.
- `net.tcp.ecn` — `true` or `false`: whether TCP connections negotiate
  Explicit Congestion Notification. `false` (the default) leaves
  connections Not-ECT; `true` offers ECN in the handshake and, once
  negotiated, treats a congestion mark as a signal to slow down instead
  of forcing a packet drop.
- `net.sockets.mem` — `auto` or a byte size such as `64M`: the memory
  the network stack may hold in socket state across every principal.
  `auto` (the default) sizes it from the machine's own RAM, so a large
  server is not held to a figure chosen on a small one; a size overrides
  that for a workload you know better. Each principal may hold a
  sixteenth of the effective budget, so there is always room for sixteen.
  Bytes rather than a socket count, because the same number of sockets is
  a few kilobytes when idle and megabytes when fully buffered: the budget
  carries many quiet connections or fewer busy ones, as the workload
  really is.
- `time.servers` — `none` or a comma-separated list of network time
  servers, each a host name or an address literal. `none` (the default)
  names no server of your own: the clock is then kept from the server the
  network's address lease names, or else from the public pool
  `pool.ntp.org`. Servers named here outrank both.
- `time.refresh` — `6h`, `12h`, `1d`, `2d`, or `7d`: how much uptime
  passes between clock re-queries once the time is known. `1d` is the
  default. A clock that is unset, implausible, or long stale is corrected
  as soon as the network allows, whatever this says.
- `time.network` — `on` (the default) or `off`: whether the clock is
  kept from the network at all. `off` sends no time query to any server,
  and the clock is set by hand or from the board's clock chip, which is
  read at every start either way. A change applies at once; the setting
  lives on the system disk, so the default applies at the start of a boot
  until that disk is reached.
- `time.zone` — `default` or a time zone's name such as `Europe/London`:
  the time zone the machine keeps, which the login screen shows and an
  account that has chosen none uses. `default` is the zone the system
  ships as its own; a name the system's list of zones does not hold is
  passed over as if it were `default`. A change applies at once.
- `input.mouse.debounce` — whole milliseconds, `25` by default, `0` to
  disable, at most `100`: how long after a mouse button is released the same
  button's next press is ignored as switch chatter instead of starting a new
  click. A worn switch can report a second press a few milliseconds after the
  release that it meant as one click. Set it to `0` for a mouse whose
  rapid-fire mode sends deliberate click pairs.
- `audio.output`, `audio.input` — `auto` by default, or an endpoint's
  location as `audioctl` lists it (sixteen hex digits, a dot, and the
  endpoint's index): the sink and the source this machine prefers as its
  defaults. A session's own choice comes first; this is the choice before
  anyone signs in and on a machine with no desktop. `auto` is the first
  device found.
- `audio.level` — decibels, `0dB` by default, or an attenuation such as
  `-12dB` or `-6.5dB`, to at most two decimals: the level every sink and
  source starts at. Never above `0dB`. The audio service takes a changed
  value at the next boot; `audioctl` changes the running machine.
- `cooling.fan.mode` — `off` by default, `on` or `auto`: whether the CPU fan
  on the board's GPIO header is left undriven, runs all the time, or is
  switched by the processor's temperature.
- `cooling.fan.line` — `none` by default, or the GPIO line the fan is wired
  to, from `0` to `63`. Only a line the board's header lends can be driven.
- `cooling.fan.signal` — `active-high` by default, or `active-low`: which
  level of the line runs the fan.
- `cooling.fan.start`, `cooling.fan.hysteresis` — whole degrees Celsius,
  `65` and `10` by default: an automatic fan starts at `start`, from `40` to
  `85`, and stops once the processor has cooled by `hysteresis`, from `2` to
  `20`. A change to a fan setting applies at once.

A key that is not in the list above is read against a second registry:
the network settings of each interface, kept at
`/System/Settings/Network/network.conf`. Its keys are spelled
`<interface>.<setting>`, where `<interface>` is the alias an
administrator gave a network interface (`wan`, `lan0`) and `<setting>`
is one of `kind`, `match.mac`, `match.node`, `ipv4.method`,
`ipv4.address`, `ipv4.gateway`, `ipv6.method`, `ipv6.address`,
`ipv6.gateway`, `dns.servers`, `mtu`, `bond.members`, `bond.mode`,
`bond.monitor-interval`, and `bond.primary`.

The machine registry above is always searched first, so an interface
alias can never take a machine setting's name over.

The network settings have no defaults, so listing shows just the
settings that are set, and showing one that is not answers with an
empty line rather than inventing a value. Reading or changing them needs
an administrator: they name each interface's hardware and this machine's
addresses. Anyone else's listing shows the machine settings and says
that the network ones are not shown.

Because that registry has no defaults, a setting is removed rather than
reset, and an **empty value** is how you remove one:
`configure wan.ipv4.address ""`. No per-interface setting accepts an
empty value, so the spelling can never be mistaken for setting one. This
is what lets one command move an interface from a static address to
DHCP — `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
— a change neither half of which describes a configuration that holds
together on its own. An interface whose last setting is removed is no
longer declared at all.

The whole edited configuration is checked before anything is saved: a
change that would leave it inconsistent (a static method with no
address, a bond with fewer than two members) is refused and nothing is
saved.

Changing a `net.*` setting, or a per-interface one, saves it and delivers
it to the running network stack, so it takes effect at once. Only the
interfaces the change actually affected are delivered. If the running
stack does not accept a delivery — none is running, or your account may
not administer the network — the setting is still saved and `configure`
says so; it then applies at the next boot.

Two limits are reported rather than hidden. An interface you removed
from the configuration keeps running with the configuration the stack was
last given until the next boot, because the stack has no message that
retires one. An interface saved with neither `match.mac` nor
`match.node` can never be bound to a device, so it is saved and the
refusal stated.

## OPTIONS

- `-h, -?` — show this command's own short help.

## EXAMPLES

- `configure` — list every setting.
- `configure os.loginType` — show the boot-default session type.
- `configure os.loginType graphical` — boot to the graphical login.
- `configure cache.all off` — disable every memory cache system-wide.
- `configure cache.filesystem off` — disable only the filesystem cache.
- `configure net.ipv6.enabled false` — turn IPv6 off stack-wide.
- `configure time.servers 0.example.test,1.example.test` — set the
  network time servers the clock is synchronised from.
- `configure time.zone Europe/London` — keep the machine on London time.
- `configure wan.ipv4.address` — show the static IPv4 address
  configured for the interface called `wan`.
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  give `wan` a static IPv4 address and default gateway.
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — move `wan` from static addressing to DHCP.
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — set the recursive
  name servers to use on `wan`.

## EXIT STATUS

- `0` — the listing, value, short help, or change was completed.
- `1` — the settings could not be read or changed (for example the
  caller may not change the machine's settings, or may not read its
  network settings), the key is unknown, the value is outside the key's
  set, the change would leave the network configuration inconsistent, or
  the output could not be delivered. A command line that was not
  understood exits `1` too.

## ENVIRONMENT

- `LANG` — the preferred locale for the short help (a BCP-47 tag such
  as `fr-FR`).

## SEE ALSO

- `man`
