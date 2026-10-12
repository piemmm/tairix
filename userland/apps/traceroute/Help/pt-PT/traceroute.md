## NAME

traceroute — mostrar a rota que as sondas de eco seguem até um anfitrião da rede

## SYNOPSIS

`traceroute [option...] host [packetlen]`

## DESCRIPTION

Envia pedidos de eco ICMP (IPv4) ou ICMPv6 (IPv6) em direção a um
anfitrião com um limite de saltos que sobe um salto de cada vez, e mostra
cada router que comunica que o limite de saltos de uma sonda se esgotou,
até o próprio anfitrião responder. As sondas passam por um socket de eco
ICMP aberto na pilha de rede em espaço de utilizador, condicionado a
`CAP_NET` e `CAP_NET_RAW` e auditado.

O destino é um endereço IPv4 ou IPv6 literal ou um nome de anfitrião,
resolvido pelo resolvedor stub do sistema; um endereço literal não precisa
de consulta.

Cada salto aparece na sua própria linha quando todas as suas sondas
ficaram decididas: o seu número e, para cada sonda, o tempo de ida e
volta, precedido do endereço — e, sem `-n`, do nome — de quem respondeu
sempre que difira da sonda anterior. Uma sonda sem resposta dentro da
espera aparece como `*`. Um router que comunica que a rota não pode
continuar marca a sua sonda — `!N` rede inalcançável, `!H` anfitrião
inalcançável, `!P` protocolo inalcançável, `!X` proibido
administrativamente, `!S` falha da rota de origem, `!F-mtu` pacote
demasiado grande, `!<code>` qualquer outro — e o traçado termina nesse
salto. Termina também quando o anfitrião responde, ou no limite de saltos.

Várias sondas ficam pendentes ao mesmo tempo (`-N`), pelo que uma série de
saltos mudos custa uma espera em vez de uma por sonda; ainda assim, um
salto só é mostrado depois de cada salto anterior o ter sido.

As sondas são pedidos de eco ICMP, o método que `-I` seleciona; uma opção
que seleciona sondas UDP ou TCP é recusada em vez de ignorada em silêncio.

## OPTIONS

- `-4` — exigir um destino IPv4.
- `-6` — exigir um destino IPv6.
- `-I, --icmp` — sondar com eco ICMP (o único método, e o predefinido).
- `-f, --first` — o limite de saltos das primeiras sondas (predefinição
  1).
- `-m, --max-hops` — o maior limite de saltos sondado (predefinição 30).
- `-q, --queries` — sondas enviadas por salto (predefinição 3, no máximo
  10).
- `-N, --sim-queries` — sondas pendentes ao mesmo tempo (predefinição 16,
  no máximo 64).
- `-w, --wait` — segundos de espera pela resposta de cada sonda (um
  decimal, predefinição 5).
- `-n` — saída numérica: só endereços, sem resolução inversa.
- `-?, --help` — mostrar a ajuda curta própria deste comando.

`packetlen` é o comprimento total de cada sonda em bytes, cabeçalhos
incluídos (predefinição 60 para IPv4 e 80 para IPv6).

## EXAMPLES

- `traceroute example.com` — traçar a rota até um anfitrião.
- `traceroute -n 192.0.2.1` — saída numérica, sem consultas de nomes.
- `traceroute -q 1 -w 1 -m 20 gateway` — uma sonda rápida por salto, no
  máximo vinte saltos.
- `traceroute -6 host.example` — traçar sobre IPv6.

## EXIT STATUS

- `0` — o traçado decorreu, tenha ou não chegado ao anfitrião (ou a ajuda
  curta foi escrita).
- `2` — a linha de comandos não foi compreendida, o destino não foi
  resolvido, o socket não pôde ser aberto, ou uma sonda não pôde ser
  enviada.

## ENVIRONMENT

- `LANG` — a localização preferida para a ajuda curta (uma etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `ping`
- `host`
- `ss`
- `man`
