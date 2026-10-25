## NAME

elsh — la shell dei comandi di TAIRiX

## SYNOPSIS

`elsh [-h | -?]`

## DESCRIPTION

Esegue una shell dei comandi interattiva — un ciclo
leggi-valuta-stampa sui flussi standard ereditati. Una parola di
comando digitata è risolta prima fra i comandi integrati della shell,
poi nello store di comandi di sistema (`/System/Commands`), nello store
di applicazioni di sistema (`/System/Applications`), nello store di
comandi (`<home>/Commands`) e nello store di applicazioni
(`<home>/Applications`) dell'utente stesso, poi nelle directory della
variabile `PATH`; questi quattro store formano un prefisso fisso che
l'utente non può riordinare né sovrascrivere, quindi `PATH` non può mai
oscurare un comando di sistema. Una parola non risolta esce con `127`;
un bundle risolto ma non eseguibile esce con `126`.

I comandi integrati:

- `cd [-L | -P [-e]] [directory]`, `cd -` — cambiare la directory di
  lavoro: in `directory`, in `HOME` se non è indicata, o con `-` di
  nuovo in `OLDPWD`, stampandola. `-L`, il comportamento predefinito,
  applica `directory` a `PWD` così com'è scritta finché `PWD` nomina
  ancora la directory di lavoro, quindi `cd link` e poi `cd ..`
  riportano dove si trova il collegamento; `-P` segue prima i
  collegamenti, e `PWD` diventa il percorso della directory stessa (`-e`
  allora fallisce quando quel percorso non può essere letto). `/..` è
  `/`, e sotto una radice alias (`Home:/`) un `..` non sale mai al di
  sopra di essa.
- `pwd [-L | -P]` — stampare la directory di lavoro: `PWD` finché la
  nomina ancora (`-L`, il comportamento predefinito), oppure il percorso
  della directory stessa (`-P`).
- `echo ...` — stampare i propri operandi.
- `export NAME=value`, `unset NAME` — modificare l'ambiente esportato.
- `jobs`, `fg`, `bg` — controllo dei job.
- `ulimit [-a] [-H | -S] [resource [value]]` — leggere e imporre limiti di
  risorse; `ulimit -n [value]` legge o imposta il limite sui file aperti.
- `umask [-S] [mode]` — mostrare la maschera di creazione del modo dei
  file (`0022`), o con `-S` i permessi che essa lascia
  (`u=rwx,g=rx,o=rx`), oppure impostarla: un `mode` ottale è la maschera
  stessa, uno simbolico (`g-w`, `u=rwx,g=rx,o=`) modifica i permessi che
  essa lascia. Ogni comando lanciato in seguito la applica ai file che
  crea, e una redirezione crea il proprio file con `0666` meno la
  maschera.
- `elevate <user> <program> [argument ...]` — eseguire un solo comando,
  con gli argomenti indicati, ri-autenticato tramite il supervisore di
  accesso della console.
- `shopt [-pqsu] [-o] [name ...]` — mostrare o impostare le opzioni dei
  modelli di nomi di file.
- `help` — elencare i comandi integrati.
- `exit [code]` — terminare la sessione.

Una parola che contiene un `*`, `?` o `[...]` non protetto è un modello
di nomi di file, sostituito, come in bash, dai nomi ordinati a cui
corrisponde: `cp *.tmp /tmp`. Un `.` iniziale corrisponde solo a un `.`,
e un modello senza corrispondenze resta com'è scritto. Le graffe si
espandono per prime: `a{b,c}` dà `ab ac`, e `{1..5}` conta. Le
virgolette rendono letterale ognuno di questi caratteri, e il valore di
una variabile non viene mai letto come modello. `shopt -s` attiva le
opzioni di bash: `nullglob` (nessuna corrispondenza non lascia alcuna
parola), `failglob` (nessuna corrispondenza è un errore), `dotglob`,
`nocaseglob`, `globstar` (`**` corrisponde a qualsiasi profondità) e
`extglob` (`?(…)`, `*(…)`, `+(…)`, `@(…)`, `!(…)`); `shopt -o -s noglob`
disattiva i modelli di nomi di file.

Su un terminale la shell offre un editor di riga interattivo con i
consueti tasti di bash/zsh: Su/Giù (o `Ctrl-P`/`Ctrl-N`) scorrono la
cronologia dei comandi della sessione, `Ctrl-R` la cerca in modo
incrementale, Sinistra/Destra/Home/Fine e `Ctrl-A`/`Ctrl-E` spostano il
cursore, `Ctrl-K`/`Ctrl-U`/`Ctrl-W`/`Ctrl-Y` tagliano e incollano,
`Ctrl-T` scambia due caratteri, `Ctrl-L` ridisegna la riga su uno
schermo ripulito, `Ctrl-C` scarta la riga in modifica e `Ctrl-D` su una
riga vuota termina la sessione. Tab completa la parola sotto il cursore:
nomi di comandi (comandi integrati e app di comando installate),
percorsi di file e — per una destinazione di redirezione o una parola
con la forma di un riferimento — riferimenti a risorse come
`sys:random`. Uno spazio dei nomi completa i suoi selettori registrati
un segmento alla volta (`state:` → `net/` → `wan/` → `link`); dove il
registro conosce solo la forma — un nome di interfaccia, una linea di
interrupt — Tab offre i nomi reali della macchina, o nulla se questa
sessione non può elencarli. `info:`/`state:`/`stats:` compaiono come
argomenti e dopo `<`, che ne legge il valore
(`cat < info:mem/physical`), ma mai dopo `>`: una tale risorsa non si
scrive mai per redirezione. L'input da pipe o da script non passa
dall'editor e si comporta allo stesso modo con o senza terminale.

La shell non accetta operandi: l'esecuzione di script non fa ancora
parte della sua grammatica.

## OPTIONS

- `-h, -?` — mostrare la guida breve di questo comando e uscire.

## EXIT STATUS

- Il codice del comando integrato `exit`, oppure `0` quando il flusso
  d'ingresso termina (o è stata mostrata la guida breve).
- `2` — l'invocazione non è stata compresa.

## ENVIRONMENT

- `HOME` — dove va `cd` senza operando.
- `PWD`, `OLDPWD` — la directory di lavoro come `cd` l'ha raggiunta, e
  quella precedente; `cd` le imposta entrambe. Un `PWD` con cui la shell
  si avvia viene mantenuto finché nomina la directory di lavoro, e
  altrimenti diventa il percorso di quella directory stessa.
- `PATH` — le directory cercate dopo il prefisso fisso di store.
- `GLOBIGNORE` — modelli separati da `:` le cui corrispondenze un
  modello di nomi di file scarta. Finché la variabile non è vuota, un
  modello di nomi di file corrisponde anche ai nomi che iniziano con `.`,
  quindi `rm *` rimuove allora anche `.profile`.
- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`), esportata a ogni comando lanciato.

## SEE ALSO

- `man`
