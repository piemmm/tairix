## NAME

wc — stampare il numero di righe, parole e byte di ogni file

## SYNOPSIS

`wc [option...] [file...]`

`wc [option...] --files0-from <file>`

## DESCRIPTION

Conta, per ogni `file`, le sue righe (caratteri di nuova riga), le sue
parole e i suoi byte, e li stampa in una riga seguita dal nome del
file. Senza `file`, o quando `file` è `-`, viene letto lo standard
input (e per la forma senza operandi non viene stampato alcun nome).
Con più di un input, viene stampata una riga finale `total` secondo
`--total`.

I selettori `-l`, `-w`, `-m`, `-c` e `-L` scelgono quali conteggi
stampare; senza alcuno, vengono stampati i conteggi di righe, parole e
byte. I conteggi appaiono sempre nell'ordine fisso: righe, parole,
caratteri, byte, larghezza massima di riga. Una parola è una sequenza
massimale di caratteri non di spaziatura. `-m` conta i caratteri UTF-8
(un byte che non è UTF-8 valido conta come byte ma non come
carattere); `-L` misura la larghezza di visualizzazione di ogni riga
in colonne di terminale, con le tabulazioni che avanzano al successivo
multiplo di 8.

`--files0-from <file>` legge l'elenco degli operandi, separati da NUL,
da `file` (`-` significa lo standard input); non può essere combinato
con operandi `file`.

Un input illeggibile viene segnalato sullo standard error e
l'esecuzione continua con l'input successivo.

Su un terminale, ogni nome che compare nell'output viene messo tra
virgolette dove una shell ne avrebbe bisogno, e ogni carattere di
controllo o altro carattere invisibile al suo interno viene scritto in
forma di escape, così che un nome non possa comandare né riordinare lo
schermo; GNU lo mostra invariato. Una pipe o un file riceve il nome byte
per byte. Un nome che contiene un carattere di nuova riga viene messo
tra virgolette anche in una pipe, come fa GNU `wc`, perché ogni
conteggio resti in una sola riga.

## OPTIONS

- `-c, --bytes` — stampare il numero di byte.
- `-m, --chars` — stampare il numero di caratteri.
- `-l, --lines` — stampare il numero di caratteri di nuova riga.
- `-w, --words` — stampare il numero di parole.
- `-L, --max-line-length` — stampare la larghezza di visualizzazione
  massima di una riga.
- `--files0-from <file>` — leggere l'elenco degli operandi separati da
  NUL da `file` (`-` lo legge dallo standard input).
- `--total <when>` — quando stampare la riga `total`: `auto` (il
  predefinito: solo con più di un input), `always`, `only` (solo il
  totale, senza etichetta) o `never`.
- `-h, -?` — mostrare la guida breve di questo comando.

## EXAMPLES

- `wc notes.txt` — stampare i conteggi di righe, parole e byte di
  `notes.txt`.
- `wc -l a b` — stampare il numero di righe di `a` e di `b`, poi il
  totale.
- `wc -L table.txt` — stampare la riga più larga di `table.txt` in
  colonne di terminale.
- `wc -c --total=only a b` — stampare solo la somma dei byte.

## EXIT STATUS

- `0` — ogni input è stato contato (o la guida breve è stata scritta).
- `1` — un input non è stato leggibile, o l'output non è stato
  consegnato. Anche una riga di comando non compresa termina con `1`.

## ENVIRONMENT

- `LANG` — la localizzazione preferita per la guida breve (un tag
  BCP-47 come `it-IT`).

## SEE ALSO

- `cat`
- `head`
- `man`
