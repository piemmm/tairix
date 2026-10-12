## NAME

traceroute — mostrar la ruta que siguen las sondas de eco hasta un host de la red

## SYNOPSIS

`traceroute [option...] host [packetlen]`

## DESCRIPTION

Envía solicitudes de eco ICMP (IPv4) o ICMPv6 (IPv6) hacia un host con un
límite de saltos que sube un salto cada vez, y muestra cada router que
informa de que el límite de saltos de una sonda se agotó, hasta que el
propio host responde. Las sondas pasan por un socket de eco ICMP abierto
en la pila de red en espacio de usuario, controlado por `CAP_NET` y
`CAP_NET_RAW` y auditado.

El destino es una dirección IPv4 o IPv6 literal o un nombre de host, que
se resuelve con el resolvedor stub del sistema; una dirección literal no
necesita consulta.

Cada salto se muestra en su propia línea cuando todas sus sondas se han
resuelto: su número y, para cada sonda, el tiempo de ida y vuelta,
precedido de la dirección —y, salvo con `-n`, el nombre— de quien
respondió siempre que difiera de la sonda anterior. Una sonda que no
obtiene respuesta dentro de la espera se muestra como `*`. Un router que
informa de que la ruta no puede seguir marca su sonda —`!N` red
inalcanzable, `!H` host inalcanzable, `!P` protocolo inalcanzable, `!X`
prohibido administrativamente, `!S` falló la ruta de origen, `!F-mtu`
paquete demasiado grande, `!<code>` cualquier otro— y el trazado se
detiene en ese salto. También se detiene cuando el host responde, o en el
límite de saltos.

Hay varias sondas pendientes a la vez (`-N`), de modo que una serie de
saltos mudos cuesta una espera en vez de una por sonda; aun así un salto
solo se muestra cuando se ha mostrado cada salto anterior.

Las sondas son solicitudes de eco ICMP, el método que selecciona `-I`; una
opción que selecciona sondas UDP o TCP se rechaza en vez de ignorarse en
silencio.

## OPTIONS

- `-4` — exigir un destino IPv4.
- `-6` — exigir un destino IPv6.
- `-I, --icmp` — sondear con eco ICMP (el único método y el
  predeterminado).
- `-f, --first` — el límite de saltos de las primeras sondas (por defecto
  1).
- `-m, --max-hops` — el mayor límite de saltos sondeado (por defecto 30).
- `-q, --queries` — sondas por salto (por defecto 3, como máximo 10).
- `-N, --sim-queries` — sondas pendientes a la vez (por defecto 16, como
  máximo 64).
- `-w, --wait` — segundos de espera para la respuesta de cada sonda (un
  decimal, por defecto 5).
- `-n` — salida numérica: solo direcciones, sin resolución inversa.
- `-?, --help` — mostrar la ayuda breve propia de esta orden.

`packetlen` es la longitud total de cada sonda en bytes, cabeceras
incluidas (por defecto 60 para IPv4 y 80 para IPv6).

## EXAMPLES

- `traceroute example.com` — trazar la ruta hasta un host.
- `traceroute -n 192.0.2.1` — salida numérica, sin consultas de nombres.
- `traceroute -q 1 -w 1 -m 20 gateway` — una sonda rápida por salto, como
  máximo veinte saltos.
- `traceroute -6 host.example` — trazar sobre IPv6.

## EXIT STATUS

- `0` — el trazado se ejecutó, alcanzara o no el host (o se mostró la
  ayuda breve).
- `2` — no se entendió la línea de órdenes, el destino no se resolvió, no
  se pudo abrir el socket o no se pudo enviar una sonda.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve (una
  etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `ping`
- `host`
- `ss`
- `man`
