## NAME

ls — elencare il contenuto delle directory

## SYNOPSIS

`ls [-aABbCcdFfGghikIlmNnopQqrRsSTtUuvXx1] [-w cols] [-I PATTERN]`
`[--block-size=SIZE] [--si] [--format=WORD] [--indicator-style=WORD]`
`[--hide=PATTERN] [--time=WORD] [--time-style=STYLE] [--sort=WORD]`
`[--quoting-style=STYLE] [--full-time] [--author] [--file-type]`
`[--group-directories-first] [--zero] [--color[=WHEN]] [--] [path...]`

## DESCRIPTION

Elenca ogni operando di percorso: le voci di un operando directory
vengono lette ed elencate (a meno che `-d` non designi la directory
stessa), qualsiasi altro operando è elencato così com'è. Senza
operandi viene elencata la directory corrente (`.`).

Le voci sono ordinate per nome (o per dimensione, la più grande per
prima, con `-S`; per marca temporale, la più recente per prima, con
`-t`; per estensione con `-X`; in ordine «versione» naturale con `-v`;
per niente — nell'ordine della directory — con `-U`; secondo una chiave
scelta per nome con `--sort`; invertite con `-r`).
`--group-directories-first` porta le directory in cima all'ordinamento.
Quando l'output è un terminale vengono disposte in colonne dimensionate
sulla larghezza del terminale, riempite dall'alto in basso (`-C`);
quando non lo è (una pipe o un file), vengono elencate con un nome per
riga. `-x` riempie invece le colonne da sinistra a destra, `-m` le
elenca separate da virgole, `-1` impone un nome per riga e `-w` imposta
la larghezza in modo esplicito. Le voci il cui nome inizia con `.` sono
nascoste salvo che sia dato `-a` o `-A`; quando delle voci vengono
nascoste, una nota è emessa sul flusso informativo standard (fd 3), mai
nell'elenco stesso.

Il formato lungo (`-l`) mostra i bit di tipo e permessi, il
proprietario e il gruppo, la dimensione, una marca temporale e poi il
nome. Proprietario e gruppo sono identificatori numerici: risolvere i
nomi degli account richiede il database utenti protetto da capacità, che
un elenco non deve pretendere; l'output corrisponde quindi al ripiego
numerico dello strumento GNU (`-n` produce lo stesso). La marca temporale
è per impostazione predefinita l'ora di modifica; `-c`, `-u` e `--time`
scelgono quale delle quattro marche è mostrata (e usata per
l'ordinamento), e `--time-style` — o `--full-time` — ne fissa il
formato. La colonna del numero di collegamenti riporta quante voci di
directory nominano il file, così come lo registra il file system stesso;
un formato che non tiene il conteggio risponde `1`.

Un collegamento simbolico appare con la lettera di tipo `l` e, nel formato
lungo, come `name -> target` — la destinazione esattamente come è
memorizzata, non risolta, cioè ciò che il collegamento contiene. Un
collegamento pendente si elenca quindi normalmente; solo una postura che
lo risolve (`-L`, o `-H` per un operando) segnala una destinazione
irraggiungibile.

I nomi sono virgolettati perché i caratteri scomodi siano visibili e si
possano reincollare senza rischi in una shell: su un terminale il valore
predefinito è `shell-escape` (un nome con spazi o metacaratteri è
virgolettato, i caratteri di controllo sono mostrati come escape
`$'…'`), altrove è `literal` (i nomi così come sono). `-N`, `-Q`, `-b` e
`--quoting-style` scelgono lo stile, e `-q` mostra i caratteri di
controllo come `?`. Con `-F` e `--file-type`, un nome che contiene `*`,
`=`, `>`, `@` o `|` è virgolettato dagli stili shell, e con `-Q` e `-b`
quel carattere riceve un escape, così il nome non può passare per un
suffisso indicatore. Il `-F` di GNU lascia un `*` senza marcatura,
perciò lì un nome che termina con esso sembra un eseguibile.

Su un terminale a colori i nomi sono colorati per tipo — directory,
eseguibili e file semplici, ciascuno nel ruolo dello schema standard —
sotto il controllo di `--color`. Il colore è solo presentazione: si
aggiunge alla resa sul terminale e mai all'output in pipe o rediretto;
a parte le sequenze di escape, l'output è identico byte per byte
all'elenco semplice, quindi il colore non cambia mai le colonne,
l'ordinamento né ciò che vede uno script.

Quando sono dati più operandi — e sempre sotto `-R` — l'elenco di ogni
directory è preceduto da un'intestazione `path:`, e i blocchi sono
separati da una riga vuota. Il percorso di un'intestazione segue la
virgolettatura dell'elenco, compreso ogni `:` che contiene.

## OPTIONS

- `-t` — ordinare per la marca temporale mostrata, la più recente per
  prima.
- `-c` — usare l'ora di modifica dei metadati (ctime): con `-l`
  mostrarla e con `-t` ordinare per essa; senza `-l`, ordinare per
  essa.
- `-u` — come `-c`, ma l'ora di accesso (atime).
- `-i, --inode` — stampare il numero di nodo di ogni voce.
- `-B, --ignore-backups` — non elencare le voci il cui nome termina con
  `~`, in ogni modalità (i backup sono nascosti anche con `-a`).
- `-I, --ignore=PATTERN` — non elencare le voci che corrispondono al
  glob `PATTERN` (ripetibile); si applica in ogni modalità. Un `.`
  iniziale corrisponde solo a un `.` nel pattern, mai a un carattere
  jolly.
- `--hide=PATTERN` — come `--ignore`, ma senza effetto quando è indicato
  `-a` o `-A`.
- `--time=WORD` — quale marca mostrare e per quale ordinare: `atime`
  (`access`, `use`), `ctime` (`status`), `mtime` (`modification`) o
  `birth` (`creation`).
- `--time-style=STYLE` — formato della marca: `locale` (predefinito),
  `long-iso`, `full-iso` o `iso`. Un `+FORMAT` personalizzato non è
  supportato.
- `--full-time` — come `-l --time-style=full-iso`.
- `-a, --all` — non nascondere le voci il cui nome inizia con `.`.
- `-A, --almost-all` — come `-a`, ma senza mai elencare `.` o `..`.
- `-d, --directory` — elencare gli operandi directory stessi, non il
  loro contenuto.
- `-F, --classify` — aggiungere `/` alle directory e `*` agli
  eseguibili.
- `-g` — formato lungo senza la colonna del proprietario; implica
  `-l`.
- `-h, --human-readable` — mostrare le dimensioni come `1.1K`, `23M`
  (potenze di 1024), sia per le dimensioni dei file del formato lungo
  sia per i blocchi di `-s`.
- `-l` — formato lungo: bit dei permessi, proprietario, gruppo,
  dimensione e poi nome.
- `-m` — nomi separati da virgole, distribuiti sulla larghezza
  dell'output.
- `-n, --numeric-uid-gid` — formato lungo con proprietario e gruppo
  numerici; implica `-l`. Proprietario e gruppo sono sempre numerici
  qui (vedi sopra), quindi coincide con `-l`.
- `-o` — formato lungo senza la colonna del gruppo; implica `-l`.
- `-p` — aggiungere `/` alle directory.
- `-N, --literal` — stampare i nomi così come sono, senza virgolette
  (`--quoting-style=literal`).
- `-Q, --quote-name` — virgolettatura in stile C: racchiudere ogni nome
  tra virgolette doppie, con escape di virgolette, barre rovesciate e
  caratteri di controllo (`--quoting-style=c`).
- `-b, --escape` — come `-Q` ma senza le virgolette circostanti e con
  gli spazi con escape (`--quoting-style=escape`).
- `--quoting-style=WORD` — come vengono virgolettati i nomi: `literal`
  (`-N`), `shell`, `shell-always`, `shell-escape`,
  `shell-escape-always`, `c` (`-Q`) o `escape` (`-b`). Il valore
  predefinito è `shell-escape` su un terminale e `literal` altrimenti;
  gli stili `locale` e `clocale` non sono supportati.
- `-q, --hide-control-chars` — mostrare i caratteri non grafici come
  `?` (il valore predefinito su un terminale); influisce solo sugli
  stili senza escape.
- `--show-control-chars` — stampare i caratteri non grafici così come
  sono (il valore predefinito quando l'output non è un terminale).
- `-r, --reverse` — invertire l'ordine di ordinamento.
- `-R, --recursive` — elencare le sottodirectory ricorsivamente.
- `-L, --dereference` — mostrare le informazioni del file che ogni
  collegamento simbolico nomina, anziché quelle del collegamento, dovunque
  ne appaia uno. Un collegamento la cui destinazione è irraggiungibile è
  segnalato sull'uscita di errore e l'elenco continua, con stato di uscita
  non nullo.
- `-H, --dereference-command-line` — dereferenziare solo i collegamenti
  simbolici nominati sulla riga di comando; quelli dentro un elenco restano
  collegamenti. Vince l'ultimo fra `-L` e `-H`.
- `--dereference-command-line-symlink-to-dir` — il comportamento
  predefinito quando nessuna opzione di formato ne impone un altro: un
  collegamento della riga di comando *a una directory* è dereferenziato,
  così `ls linkdir` elenca la directory, mentre ogni altro collegamento
  resta un collegamento. `-l`, `-d` e `-F` mostrano invece per
  impostazione predefinita ogni collegamento come tale.
- `-s, --size` — stampare la dimensione allocata di ogni voce in blocchi
  (scalata da `-h` / `--si` / `--block-size` / `-k`), con una riga
  `total` per ogni directory elencata.
- `-C` — elencare in colonne, riempite dall'alto in basso
  (predefinito quando l'output è un terminale).
- `-S` — ordinare per dimensione, la più grande per prima.
- `-U` — non ordinare; elencare le voci nell'ordine della directory.
- `-X` — ordinare per estensione del nome (il testo dall'ultimo `.`),
  a parità per nome.
- `-v` — ordinamento «versione» naturale, così `f2` precede `f10`;
  a parità per nome.
- `-f` — non ordinare e mostrare tutte le voci: attiva `-a` e `-U` e
  disattiva `-l` e `-s`. Applicato nella sua posizione, quindi un
  successivo `-l`/`-s`/flag di ordinamento lo sovrascrive.
- `--sort=WORD` — scegliere la chiave di ordinamento per nome: `none`
  (`-U`), `size` (`-S`), `time` (`-t`), `version` (`-v`), `extension`
  (`-X`) o `name`.
- `--group-directories-first` — elencare le directory prima delle
  altre voci; le directory per prime anche con `-r`.
- `-w, --width <cols>` — impostare la larghezza di output in colonne;
  `0` significa una lunghezza di riga illimitata. Senza di essa si usa la
  larghezza del terminale, o 80 quando non può essere determinata.
- `-x` — elencare in colonne, riempite da sinistra a destra.
- `-1` — un nome per riga (l'impostazione predefinita quando l'output
  non è un terminale).
- `-?` — mostrare la guida breve di questo comando (`--help` è la
  forma lunga).
- `--file-type` — aggiungere `/` alle directory, ma mai `*` agli
  eseguibili (`--indicator-style=file-type`).
- `--indicator-style=WORD` — scegliere il suffisso indicatore per nome:
  `none`, `slash` (`-p`), `file-type` (`--file-type`) o `classify`
  (`-F`).
- `-G, --no-group` — omettere la colonna del gruppo nel formato lungo;
  a differenza di `-o` non seleziona da solo il formato lungo.
- `--author` — con `-l`, stampare la colonna autore (l'utente
  proprietario, poiché non esiste un autore distinto) dopo il
  proprietario e prima del gruppo.
- `--si` — come `-h` ma in potenze di 1000 (`1.1k`, `23M`).
- `-k, --kibibytes` — usare blocchi da 1024 byte per le celle `-s` e
  il `total`. È già il valore predefinito, quindi conferma l'output
  anziché cambiarlo; un'opzione di dimensione ha la precedenza.
- `--block-size=SIZE` — scalare le dimensioni dei file del formato lungo
  e i blocchi `-s` di SIZE: un intero semplice (byte), o un'unità
  `K`/`M`/`G`/`T`/`P`/`E` (base 1024), un'unità in stile `KiB` (base
  1024) o un'unità in stile `KB` (base 1000), facoltativamente preceduta
  da un coefficiente intero. Un'unità da sola stampa il suo suffisso; un
  coefficiente lo sopprime. Un SIZE malformato è un errore.
- `--format=WORD` — scegliere la disposizione per nome: `long` (`-l`) o
  `verbose`, `single-column` (`-1`), `vertical` (`-C`), `across` o
  `horizontal` (`-x`), o `commas` (`-m`).
- `-T, --tabsize <cols>` — impostare il passo di tabulazione della
  griglia di colonne (predefinito 8); `0` riempie solo con spazi.
- `--zero` — terminare ogni riga con NUL invece del ritorno a capo;
  seleziona inoltre, salvo diversa indicazione, la colonna singola, la
  virgolettatura letterale e i caratteri di controllo visibili.

- `--color[=WHEN]` — colorare i nomi per tipo (directory, eseguibili,
  file semplici). `WHEN` è `auto` (il valore predefinito: colorare solo
  quando l'output è un terminale attestato), `always` (colorare anche
  quando non lo è, ad es. una console seriale) o `never`; `--color`
  senza `WHEN` equivale a `always`. L'output in pipe o rediretto non
  viene mai colorato.

## EXAMPLES

- `ls` — elencare la directory corrente.
- `ls -al /System` — elenco in formato lungo di `/System`, voci
  nascoste comprese.
- `ls -lhS` — formato lungo, dimensioni leggibili, la più grande per
  prima.
- `ls -v` — ordinamento naturale, così `f2` viene prima di `f10`.
- `ls --group-directories-first` — prima le directory, poi i file.
- `ls -R Documents` — attraversare `Documents` ricorsivamente,
  un'intestazione per directory.
- `ls -F` — contrassegnare le directory con `/` e gli eseguibili con
  `*`.
- `ls -d Documents` — elencare la voce `Documents` stessa, non il suo
  contenuto.

## EXIT STATUS

- `0` — ogni operando è stato elencato.
- `1` — un problema minore: una voce o una sottodirectory dentro un
  elenco non è stata ispezionata o letta.
- `2` — un problema grave: la riga di comando non è stata compresa, un
  operando non è stato ispezionato o letto, `-R` ha raggiunto di nuovo
  una directory attraverso un collegamento, la memoria si è esaurita,
  oppure l'output non è stato consegnato.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

- `TERM` — il tipo di terminale, che decide la profondità di colore
  dell'output `--color`. Un `TERM` non impostato o senza colore produce
  testo semplice con `auto`.

## SEE ALSO

- `cat`
- `man`
