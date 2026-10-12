## NAME

traceroute — den Weg ausgeben, den Echo-Sonden zu einem Netzwerkhost nehmen

## SYNOPSIS

`traceroute [option...] host [packetlen]`

## DESCRIPTION

Sendet ICMP- (IPv4) oder ICMPv6-Echoanfragen (IPv6) mit einem Hop-Limit,
das Hop um Hop steigt, in Richtung eines Hosts und gibt jeden Router aus,
der meldet, dass das Hop-Limit einer Sonde abgelaufen ist, bis der Host
selbst antwortet. Die Sonden laufen über einen ICMP-Echo-Socket des
Netzwerkstacks im Benutzerraum, freigegeben durch `CAP_NET` und
`CAP_NET_RAW` und protokolliert.

Das Ziel ist eine IPv4- oder IPv6-Adresse oder ein Hostname, der über den
Stub-Resolver des Systems aufgelöst wird; eine Adresse braucht keine
Abfrage.

Jeder Hop erscheint in einer eigenen Zeile, sobald alle seine Sonden
entschieden sind: seine Nummer, dann für jede Sonde die Umlaufzeit, davor
die Adresse — und ohne `-n` der Name — des Antwortenden, wann immer er
sich von der vorigen Sonde unterscheidet. Eine Sonde ohne Antwort
innerhalb der Wartezeit erscheint als `*`. Meldet ein Router, dass der Weg
nicht weitergeht, wird seine Sonde markiert — `!N` Netz unerreichbar, `!H`
Host unerreichbar, `!P` Protokoll unerreichbar, `!X` administrativ
verboten, `!S` Quellroute gescheitert, `!F-mtu` Paket zu groß, `!<code>`
alles andere — und die Verfolgung endet bei diesem Hop. Sie endet auch,
sobald der Host antwortet, oder an der Hop-Grenze.

Mehrere Sonden sind gleichzeitig unterwegs (`-N`), sodass eine Folge
stummer Hops eine Wartezeit kostet statt einer pro Sonde; ein Hop wird
dennoch erst ausgegeben, wenn jeder Hop davor ausgegeben ist.

Die Sonden sind ICMP-Echoanfragen, die Methode, die `-I` wählt; eine
Option, die UDP- oder TCP-Sonden wählt, wird abgelehnt statt
stillschweigend ignoriert.

## OPTIONS

- `-4` — ein IPv4-Ziel verlangen.
- `-6` — ein IPv6-Ziel verlangen.
- `-I, --icmp` — mit ICMP-Echo sondieren (die einzige Methode und der
  Standard).
- `-f, --first` — das Hop-Limit der ersten Sonden (Standard 1).
- `-m, --max-hops` — das höchste sondierte Hop-Limit (Standard 30).
- `-q, --queries` — Sonden pro Hop (Standard 3, höchstens 10).
- `-N, --sim-queries` — gleichzeitig ausstehende Sonden (Standard 16,
  höchstens 64).
- `-w, --wait` — Sekunden, die auf die Antwort jeder Sonde gewartet wird
  (eine Dezimalzahl, Standard 5).
- `-n` — numerische Ausgabe: nur Adressen, ohne Rückwärtsauflösung.
- `-?, --help` — die eigene Kurzhilfe dieses Befehls anzeigen.

`packetlen` ist die Gesamtlänge jeder Sonde in Bytes, Kopfdaten
eingeschlossen (Standard 60 für IPv4 und 80 für IPv6).

## EXAMPLES

- `traceroute example.com` — den Weg zu einem Host verfolgen.
- `traceroute -n 192.0.2.1` — numerische Ausgabe, keine Namensabfragen.
- `traceroute -q 1 -w 1 -m 20 gateway` — eine schnelle Sonde pro Hop,
  höchstens zwanzig Hops.
- `traceroute -6 host.example` — über IPv6 verfolgen.

## EXIT STATUS

- `0` — die Verfolgung lief, ob sie den Host erreichte oder nicht (oder
  die Kurzhilfe wurde ausgegeben).
- `2` — die Befehlszeile wurde nicht verstanden, das Ziel ließ sich nicht
  auflösen, der Socket ließ sich nicht öffnen, oder eine Sonde ließ sich
  nicht senden.

## ENVIRONMENT

- `LANG` — die bevorzugte Sprache für die Kurzhilfe (ein BCP-47-Tag wie
  `fr-FR`).

## SEE ALSO

- `ping`
- `host`
- `ss`
- `man`
