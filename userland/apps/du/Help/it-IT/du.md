## NAME

du — stimare lo spazio su disco usato dai file

## SYNOPSIS

`du [option...] [file...]`

## DESCRIPTION

Attraversa ogni operando `file` e stampa, per directory (la più
profonda per prima), lo spazio di archiviazione occupato dall'albero
sottostante, come `size<TAB>path`. Senza `file` viene attraversata la
directory corrente (`.`). Un operando `file` che non è una directory
viene stampato da solo.

La misura predefinita è lo spazio realmente allocato di ogni nodo,
così come lo riporta il filesystem montato; i file sparsi o compressi
contano quindi ciò che occupano davvero. `--apparent-size` (o `-b`)
misura invece le lunghezze apparenti in byte. Le dimensioni sono
stampate in blocchi da 1024 byte salvo che un'opzione di unità scelga
diversamente; un'opzione di unità successiva sostituisce la
precedente, e i conteggi di blocchi sono arrotondati per eccesso (un
blocco parzialmente usato è un blocco usato).

Un percorso illeggibile viene segnalato sull'errore standard e
l'attraversamento continua con il resto; una directory illeggibile
non contribuisce nulla anziché una somma parziale indovinata.

Un collegamento simbolico è contato in quanto tale, a meno che `-H` o
`-L` non dica di seguirlo. Tutto ciò che si incontra una seconda volta —
un altro nome di un file già contato, un operando ripetuto o situato
dentro un altro, una directory che `-L` raggiunge due volte — viene
tralasciato, senza una riga propria, così il suo spazio non è riportato
due volte; `-l` conta invece ogni incontro. `-x` (un solo filesystem)
non è ancora disponibile; le variabili d'ambiente della famiglia
`DU_BLOCK_SIZE` non vengono lette — la scala si sceglie solo con le
opzioni.

Su un terminale, ogni nome mostrato dall'output è virgolettato dove una
shell lo richiederebbe, e ogni carattere di controllo o altro carattere
invisibile che contiene è scritto come sequenza di escape, così un nome
non può comandare lo schermo né riordinarlo; GNU lo mostra invariato.
Una pipe, un file o l'output di `-0` ricevono il nome byte per byte.

## OPTIONS

- `-a, --all` — riportare anche ogni file, non solo le directory.
- `-s, --summarize` — riportare solo il totale di ogni operando (in
  conflitto con `-a` e `-d`).
- `-c, --total` — aggiungere una riga di totale generale etichettata
  `total`.
- `-d, --max-depth <n>` — riportare le directory fino a `n` livelli
  sotto un operando (`0` riporta solo gli operandi); i totali non
  cambiano.
- `-S, --separate-dirs` — la riga di una directory esclude le sue
  sottodirectory.
- `-l, --count-links` — contare un file con più nomi una volta per
  nome invece di una sola.
- `-L, --dereference` — seguire ogni collegamento simbolico.
- `-H, -D, --dereference-args` — seguire solo i collegamenti simbolici
  indicati come operandi.
- `-P, --no-dereference` — non seguire alcun collegamento simbolico (il
  valore predefinito).
- `--apparent-size` — misurare le lunghezze apparenti in byte, non lo
  spazio allocato.
- `-b, --bytes` — dimensione apparente in singoli byte
  (`--apparent-size` con dimensione di blocco 1).
- `-k` — blocchi da 1024 byte (il valore predefinito).
- `-m` — blocchi da 1048576 byte.
- `-h, --human-readable` — dimensioni leggibili in potenze di 1024
  (`1.0K`, `23M`).
- `--si` — dimensioni leggibili in potenze di 1000 (`1.0k`, `23M`).
- `-B, --block-size <size>` — riportare in blocchi di `size` byte
  (`512`, `1K`, `1MiB`, `1GB`, `human-readable`, `si`).
- `-0, --null` — terminare ogni riga con NUL invece del ritorno a
  capo.
- `-?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `du` — l'albero della directory corrente, una riga per directory.
- `du -sh /Users/jo` — un totale leggibile per `/Users/jo`.
- `du -a docs` — ogni file e directory sotto `docs`.
- `du -d1 -c /System/Applications /Users` — il primo livello di ogni archivio, poi
  un totale generale.

## EXIT STATUS

- `0` — ogni operando è stato attraversato (o la guida breve è stata
  scritta).
- `1` — un percorso non è stato leggibile, o l'output non è stato
  consegnato. Anche una riga di comando non compresa termina con `1`.

## ENVIRONMENT

- `LANG` — la lingua preferita per la guida breve (un'etichetta
  BCP-47 come `fr-FR`).

## SEE ALSO

- `df`
- `ls`
- `man`
