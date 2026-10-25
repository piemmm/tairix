## NAME

cp — copiare file e directory

## SYNOPSIS

`cp [-dfHilLnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

## DESCRIPTION

Copia ogni operando sorgente verso una destinazione. Con una sola
sorgente e una destinazione che non nomina una directory, la sorgente
è copiata su quel percorso esatto. Quando la destinazione nomina una
directory esistente — e sempre quando c'è più di una sorgente — ogni
sorgente è copiata *dentro* quella directory con il proprio nome base.

Una sorgente directory è copiata solo con `-r`, che riproduce
l'intero sottoalbero; senza `-r` un operando directory è rifiutato.
Un file di destinazione esistente è sovrascritto per impostazione
predefinita, saltato con `-n` e richiesto sul flusso di errore
standard con `-i` (una domanda rifiutata salta quella copia e fa
fallire l'esecuzione, come in GNU; una risposta illeggibile non vale
mai come consenso).
Una destinazione che è il file sorgente stesso — lo stesso nome, un
altro nome del file o un collegamento a esso — viene rifiutata in
quanto stesso file e resta com'era, tranne con `-l`, che la considera
già collegata. Un collegamento che `-f` mette al posto di un nome
esistente prende quel posto in un solo passo ed è creato prima che si
sostituisca alcunché, così uno che non si può creare lascia intatto
il nome.

Un file che non può essere letto, creato o scritto viene segnalato e
gli altri vengono copiati comunque, anche il resto della sua
directory. `--` termina l'analisi delle opzioni: ogni argomento
successivo è un percorso.

Un nuovo file è creato con i bit di permesso della sorgente, senza i bit
set-ID e sticky, al netto della umask, così la copia di un file privato
resta privata; una destinazione che esiste già mantiene il proprio modo.
Una nuova directory riceve alla fine i bit di permesso e sticky della
sorgente, al netto della umask; mentre vi si copiano le voci resta
aperta al suo proprietario, così la copia è completa anche se la
directory sorgente è di sola lettura o se la umask toglie la scrittura
al proprietario. Attraverso un collegamento simbolico incontrato dentro
un albero di destinazione non si scrive mai.

L'opzione `-a`/`--archive` di GNU e i valori di `--preserve` diversi da
`--preserve=links,xattr` sono **rifiutati**, non ristretti. `-a`
equivale a `-dR --preserve=all`, e `--preserve=all` include le marche
temporali di un nodo, che nessuna chiamata di questo sistema può
impostare — perciò onorare `-a` riferirebbe una conservazione mai
avvenuta. Per il resto usate `-dR`.

Ogni copia porta con sé gli attributi estesi della sorgente — un tipo di
file RISC OS, il tipo e il creatore di un Mac — ovunque la destinazione
possa conservarli, e mantiene come buchi i buchi di un file sparso. Gli
attributi che una destinazione non può conservare vengono segnalati in
un unico avviso al termine dell'esecuzione, e in un record `omission`
sul flusso di informazioni standard; con `--preserve=xattr` una copia
del genere fallisce invece.

Una directory non viene mai copiata dentro sé stessa: una destinazione
dentro la sorgente — comunque sia scritta, attraverso un collegamento,
un `..` o un altro nome del volume — viene rifiutata prima che si scriva
alcunché, e una directory spostata nella sorgente durante la copia ferma
la copia dove la visita la incontra.

## OPTIONS

- `-r, -R, --recursive` — copiare le directory e il loro contenuto.
- `-f, --force` — quando un file di destinazione non può essere
  creato, rimuoverlo e ritentare la copia una volta; con `-l`, `-s` o
  un collegamento che `-P` riproduce, mettere il collegamento al posto
  di un nome esistente.
- `-i, --interactive` — chiedere prima di sovrascrivere un file
  esistente; acconsente solo una risposta che inizia con `y`/`Y`, e
  una domanda rifiutata salta quel file e fa fallire l'esecuzione.
- `-n, --no-clobber` — non sovrascrivere mai un file esistente. Vince
  l'ultimo tra `-i` e `-n`.
- `-l, --link` — dare alla destinazione un secondo nome del nodo della
  sorgente invece di copiarne i byte, così i due nomi non possono
  divergere a una scrittura successiva. Una sorgente che è directory
  richiede comunque `-r`.
- `-s, --symbolic-link` — creare un collegamento simbolico che nomina la
  sorgente invece di copiarla.
- `-P, --no-dereference` — riprodurre ogni collegamento simbolico tra le
  sorgenti come un collegamento con la stessa destinazione, alla
  lettera, invece di copiare ciò a cui punta (così un collegamento
  relativo o pendente sopravvive alla copia).
- `-L, --dereference` — seguire ogni collegamento simbolico tra le
  sorgenti e copiare ciò a cui punta.
- `-H` — seguire un collegamento simbolico indicato come operando, ma
  riprodurre quelli incontrati dentro un albero. Senza `-H`, `-L` o
  `-P`, una copia ricorsiva riproduce ogni collegamento, compresi quelli
  dei suoi operandi, e ogni altra copia segue i collegamenti dei suoi
  operandi, come quella di GNU; `-l` collega ciò a cui punta un
  collegamento seguito.
- `--preserve=links` — due sorgenti che nominano un solo nodo ottengono
  due nomi alla destinazione invece di due copie, così la copia non
  raddoppia in silenzio lo spazio.
- `--preserve=xattr` — ogni attributo esteso deve essere copiato: una
  copia la cui destinazione non può conservarne uno fallisce,
  indicandolo. `--preserve=links,xattr` richiede entrambe le cose.
- `--no-preserve=xattr` — non copiare alcun attributo esteso. Sono
  accettati anche `ownership` e `timestamps`, che già non vengono
  preservati.
- `--no-preserve=mode` — creare ogni nuovo file e ogni nuova directory
  con il modo che un nuovo oggetto assume quando nessuno ne indica uno,
  al netto della umask, anziché con quello della sorgente. Anche
  `--no-preserve=all` fa lo stesso.
- `--sparse=WHEN` — come viene copiato un file sparso: `auto`, il
  predefinito, ne mantiene i buchi; `always` lascia inoltre non scritto
  ogni blocco di zeri che contiene; `never` scrive ogni byte.
- `-d` — `-P` e `--preserve=links` insieme, come nello strumento GNU.
- `-v, --verbose` — riferire ogni copia come `'source' -> 'dest'`.
- `-t dir, --target-directory=dir` — copiare ogni sorgente dentro
  `dir`, che deve essere una directory esistente. Il valore segue
  attaccato (`-tdir`, `--target-directory=dir`) o come argomento
  successivo.
- `-T, --no-target-directory` — trattare la destinazione come un file
  normale; è permessa esattamente una sorgente. Non combinabile con
  `-t`.
- `-h, -?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `cp notes.txt backup.txt` — copiare un file con un nuovo nome.
- `cp -r Projects Archive` — riprodurre l'albero `Projects` dentro
  `Archive` (o come `Archive` se non esiste).
- `cp -v -t Backup a.txt b.txt` — copiare entrambi i file in
  `Backup`, riferendo ogni copia.

## EXIT STATUS

- `0` — ogni copia è riuscita (un salto per `-n` non è un fallimento).
- `1` — un file non si è potuto copiare, una domanda `-i` è stata
  rifiutata, o la domanda o l'output non sono riusciti; il motivo di
  ogni fallimento è stampato sul flusso di errore standard. Anche una
  riga di comando non compresa termina con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
