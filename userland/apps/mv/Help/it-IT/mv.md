## NAME

mv — spostare (rinominare) file e directory

## SYNOPSIS

`mv [-finvT] [-t dir] [--] source... dest`

## DESCRIPTION

Sposta ogni operando sorgente verso una destinazione. Con una sola
sorgente e una destinazione che non nomina una directory, la sorgente
è rinominata su quel percorso esatto. Quando la destinazione nomina
una directory esistente — e sempre quando c'è più di una sorgente —
ogni sorgente è spostata *dentro* quella directory con il proprio
nome base.

Uno spostamento all'interno di un volume è una rinomina atomica che
conserva l'identità del nodo. Uno spostamento tra volumi mantiene la
stessa promessa: la copia viene costruita accanto alla destinazione,
leggibile solo dal suo proprietario finché non è completa, scritta sul
disco e rinominata al suo posto in un solo passo, e solo allora ciò che
è stato copiato viene rimosso — così la destinazione conserva ciò che
conteneva finché lo spostamento non è completo, e né un errore né un
arresto anomalo perdono qualcosa. Un file aggiunto alla sorgente, o
modificato, mentre veniva copiato resta dov'era insieme alla directory
che lo contiene, e viene segnalato; su un volume che non registra un
contatore delle modifiche, o che registra i tempi a grana grossa (i due
secondi di FAT), una riscrittura della stessa lunghezza fatta entro
quella granularità può passare inosservata. In entrambi i casi una
directory sostituisce solo una directory vuota, nient'altro sostituisce
una directory e nessuna directory viene spostata nel proprio
sottoalbero.

Una destinazione esistente è sovrascritta per impostazione predefinita,
mantenuta con `-n` (che segnala `not replacing` e fa fallire
l'esecuzione, come in GNU) e richiesta sul flusso di errore standard con
`-i` (una domanda rifiutata salta quello spostamento e fa fallire
l'esecuzione; una risposta illeggibile non vale mai come consenso). Un
operando che non si può spostare viene segnalato, e il successivo viene
spostato comunque. `--` termina l'analisi delle opzioni: ogni argomento
successivo è un percorso.

La copia conserva i buchi di ogni file, i suoi attributi estesi — un
tipo di file RISC OS, un tipo e un creatore Mac —, i permessi e il
proprietario ovunque la destinazione li mantenga, e sposta un
collegamento simbolico come un collegamento con la stessa destinazione,
mai ciò a cui punta. Un file restituito a un altro proprietario arriva
senza i bit set-ID, che il sistema azzera quando il proprietario cambia;
uno che chi lo sposta non può restituire diventa di chi lo sposta,
mantenendo il gruppo che aveva dove chi lo sposta può mantenerlo, e
senza i bit set-ID di un proprietario o di un gruppo che non ha
mantenuto. Le marche temporali non vengono conservate: nessuna chiamata
di questo sistema le imposta. Gli attributi che la destinazione non può
mantenere vengono segnalati in un unico avviso al termine
dell'esecuzione, e in un record `omission` sul flusso di informazioni
standard, prima che le sorgenti scompaiano per sempre.

## OPTIONS

- `-f, --force` — non chiedere mai prima di sovrascrivere. Vale l'ultimo
  tra `-f`, `-i` e `-n`.
- `-i, --interactive` — chiedere prima di sovrascrivere una
  destinazione esistente; acconsente solo una risposta che inizia con
  `y`/`Y`.
- `-n, --no-clobber` — non sovrascrivere mai una destinazione esistente;
  ciascuna destinazione mantenuta è riportata come
  `not replacing 'dest'`.
- `-v, --verbose` — riportare ogni spostamento come
  `renamed 'source' -> 'dest'`, e uno spostamento tra volumi voce per
  voce, come fa GNU: `created directory 'dest'` per ogni directory e
  `copied 'source' -> 'dest'` per tutto il resto man mano che viene
  copiato, poi `removed 'source'` o `removed directory 'source'` per
  ciascuna voce man mano che la sorgente scompare.
- `-t dir, --target-directory=dir` — spostare ogni sorgente dentro
  `dir`, che deve essere una directory esistente. Il valore segue
  attaccato (`-tdir`, `--target-directory=dir`) o come argomento
  successivo.
- `-T, --no-target-directory` — trattare la destinazione come un file
  normale; è permessa esattamente una sorgente. Non combinabile con
  `-t`.
- `-h, -?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `mv draft.txt final.txt` — rinominare un file.
- `mv -v a.txt b.txt Archive` — spostare entrambi i file in
  `Archive`, riferendo ogni spostamento.
- `mv -n new.cfg current.cfg` — installare un file solo se la
  destinazione non esiste già (riportando `not replacing` se esiste).

## EXIT STATUS

- `0` — ogni spostamento è riuscito.
- `1` — un operando non si è potuto spostare, `-n` ha mantenuto una
  destinazione, una domanda `-i` è stata rifiutata, qualcosa in una
  sorgente è cambiato durante lo spostamento ed è stato mantenuto, o la
  domanda o l'output non sono riusciti; il motivo di ogni fallimento è
  stampato sul flusso di errore standard. Anche una riga di comando non
  compresa termina con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
