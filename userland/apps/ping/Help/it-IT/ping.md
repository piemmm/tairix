## NAME

ping — inviare richieste di eco ICMP a un host di rete

## SYNOPSIS

`ping [option...] host`

## DESCRIPTION

Invia richieste di eco ICMP (IPv4) o ICMPv6 (IPv6) a un host e mostra
ogni risposta con il suo tempo di andata e ritorno, seguito da un
riepilogo finale.

Le richieste passano per un socket di eco ICMP aperto sullo stack di rete
in spazio utente, protetto da `CAP_NET` e `CAP_NET_RAW` e registrato. Lo
stack possiede l'identificatore di eco, così un socket riceve solo le
risposte alle proprie richieste.

La destinazione è un indirizzo IPv4 o IPv6 letterale oppure un nome host.
Un nome viene risolto dal resolver di sistema, usando i server ricorsivi
configurati sulla macchina; un indirizzo letterale non richiede alcuna
interrogazione e funziona quindi anche senza resolver configurato. Un nome
che non risolve ad alcun indirizzo della famiglia richiesta termina
l'esecuzione indicandone la ragione.

Per impostazione predefinita ogni richiesta porta dati casuali ad alta
entropia, estratti di nuovo per ogni richiesta. È deliberato: un
collegamento che comprime o deduplica il traffico riporterebbe altrimenti
una velocità e una latenza che nulla dicono della sua capacità reale. I
byte restituiti sono confrontati con quelli inviati, così un payload
casuale è anche una verifica d'integrità per pacchetto. Con `-p` si sceglie
un motivo fisso quando serve un payload deterministico.

Per impostazione predefinita `ping` invia una richiesta al secondo fino
all'interruzione; `-c` ne limita il numero. Ogni risposta indica origine,
numero di sequenza e tempo. Quando un router, o l'host stesso, segnala un
errore relativo a una richiesta — il limite di hop esaurito, la
destinazione irraggiungibile, il pacchetto troppo grande — una riga `From`
indica chi lo segnala e il motivo. Una richiesta che non riceve risposta
entro il limite non stampa nulla. Se nessuna interfaccia può ancora
trasportare una richiesta, questa attende fino al proprio limite che la
rete sia pronta. Il riepilogo finale indica i pacchetti trasmessi e
ricevuti, gli eventuali errori, la percentuale di perdita e i tempi di
andata e ritorno minimo, medio e massimo con la loro deviazione media.
`-q` mostra solo l'intestazione e il riepilogo.

Ogni risposta nomina il pari come `nome (indirizzo)` quando l'indirizzo
ha un record `PTR`, risolto una sola volta per esecuzione con lo stesso
resolver; un indirizzo senza nome, e ogni esecuzione sotto `-n`, stampa
l'indirizzo nudo. `-n` significa inoltre che nessuna query `PTR` viene
messa sulla rete.

Ogni riga di risposta porta il TTL IPv4 (o il limite di hop IPv6) con cui
la risposta è arrivata in un campo `ttl=`, come fa `iputils`.

## OPTIONS

- `-c, --count` — fermarsi dopo questo numero di richieste.
- `-i, --interval` — secondi tra le richieste (un decimale, es. `0.5`).
- `-s, --size` — dimensione del payload in byte.
- `-p, --pattern` — contenuto del payload: `random` (predefinito, alta
  entropia) o una sequenza di cifre esadecimali di lunghezza pari come
  motivo di byte ripetuto, p. es. `-p ff00`.
- `-W, --timeout` — secondi di attesa per ogni risposta.
- `-w, --deadline` — scadenza complessiva dell'esecuzione, in secondi.
- `-4, --ipv4` — richiedere una destinazione IPv4.
- `-6, --ipv6` — richiedere una destinazione IPv6.
- `-n, --numeric` — output numerico: non risolvere il pari all'indietro,
  quindi nessuna query `PTR` viene emessa e le righe di risposta portano
  l'indirizzo nudo.
- `-q, --quiet` — silenzioso: solo l'intestazione e il riepilogo finale.
- `-?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `ping 10.0.2.2` — pingare un host IPv4 fino all'interruzione.
- `ping -c 4 fe80::1` — inviare quattro richieste a un host IPv6.
- `ping -c 10 -i 0.2 10.0.0.1` — dieci richieste, una ogni 200 ms.
- `ping -q -c 100 10.0.0.1` — esecuzione silenziosa, solo riepilogo.

## EXIT STATUS

- `0` — è stata ricevuta almeno una risposta (o è stata scritta la guida).
- `1` — nessuna richiesta ha ricevuto risposta. Anche una riga di
  comando non compresa termina con `1`.
- `2` — la destinazione non si è risolta, il socket non si è potuto
  aprire, o una risposta non si è potuta ricevere o scrivere.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `fr-FR`).

## SEE ALSO

- `host`
- `ss`
- `sysinfo`
- `man`
