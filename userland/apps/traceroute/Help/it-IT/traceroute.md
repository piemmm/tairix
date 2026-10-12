## NAME

traceroute — mostrare il percorso che le sonde di eco seguono verso un host di rete

## SYNOPSIS

`traceroute [option...] host [packetlen]`

## DESCRIPTION

Invia richieste di eco ICMP (IPv4) o ICMPv6 (IPv6) verso un host con un
limite di hop che sale di un hop alla volta, e mostra ogni router che
segnala l'esaurimento del limite di hop di una sonda, finché l'host stesso
risponde. Le sonde passano per un socket di eco ICMP aperto nello stack di
rete in spazio utente, soggetto a `CAP_NET` e `CAP_NET_RAW` e verificato.

La destinazione è un indirizzo IPv4 o IPv6 letterale o un nome host,
risolto tramite il resolver stub di sistema; un indirizzo letterale non
richiede alcuna interrogazione.

Ogni hop compare su una riga propria quando tutte le sue sonde sono
decise: il suo numero, poi per ogni sonda il tempo di andata e ritorno,
preceduto dall'indirizzo — e, senza `-n`, dal nome — di chi ha risposto
ogni volta che differisce dalla sonda precedente. Una sonda senza risposta
entro l'attesa compare come `*`. Un router che segnala che il percorso non
può proseguire marca la sua sonda — `!N` rete irraggiungibile, `!H` host
irraggiungibile, `!P` protocollo irraggiungibile, `!X` proibito
amministrativamente, `!S` instradamento d'origine fallito, `!F-mtu`
pacchetto troppo grande, `!<code>` qualsiasi altro — e la traccia si ferma
a quell'hop. Si ferma anche quando l'host risponde, o al limite di hop.

Più sonde sono in sospeso contemporaneamente (`-N`), così una serie di hop
muti costa un'attesa invece di una per sonda; un hop viene comunque
mostrato solo quando ogni hop precedente è stato mostrato.

Le sonde sono richieste di eco ICMP, il metodo che `-I` seleziona;
un'opzione che seleziona sonde UDP o TCP viene rifiutata invece che
ignorata in silenzio.

## OPTIONS

- `-4` — richiedere una destinazione IPv4.
- `-6` — richiedere una destinazione IPv6.
- `-I, --icmp` — sondare con eco ICMP (l'unico metodo, e quello
  predefinito).
- `-f, --first` — il limite di hop delle prime sonde (predefinito 1).
- `-m, --max-hops` — il limite di hop più alto sondato (predefinito 30).
- `-q, --queries` — sonde inviate per hop (predefinito 3, al massimo 10).
- `-N, --sim-queries` — sonde in sospeso contemporaneamente (predefinito
  16, al massimo 64).
- `-w, --wait` — secondi di attesa della risposta di ogni sonda (un
  decimale, predefinito 5).
- `-n` — output numerico: solo indirizzi, senza risoluzione inversa.
- `-?, --help` — mostrare l'aiuto breve di questo comando.

`packetlen` è la lunghezza totale di ogni sonda in byte, intestazioni
comprese (predefinita 60 per IPv4 e 80 per IPv6).

## EXAMPLES

- `traceroute example.com` — tracciare il percorso verso un host.
- `traceroute -n 192.0.2.1` — output numerico, nessuna ricerca di nomi.
- `traceroute -q 1 -w 1 -m 20 gateway` — una sonda rapida per hop, al
  massimo venti hop.
- `traceroute -6 host.example` — tracciare su IPv6.

## EXIT STATUS

- `0` — la traccia è stata eseguita, che abbia raggiunto l'host o no (o è
  stato scritto l'aiuto breve).
- `2` — la riga di comando non è stata compresa, la destinazione non si è
  risolta, il socket non si è potuto aprire, o una sonda non si è potuta
  inviare.

## ENVIRONMENT

- `LANG` — la localizzazione preferita per l'aiuto breve (un tag BCP-47
  come `fr-FR`).

## SEE ALSO

- `ping`
- `host`
- `ss`
- `man`
