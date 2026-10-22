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
scritta sul disco e rinominata al suo posto in un solo passo, e solo
allora la sorgente viene rimossa — così la destinazione conserva ciò che
conteneva finché lo spostamento non è completo, e né un errore né un
arresto anomalo perdono qualcosa. In entrambi i casi una directory
sostituisce solo una directory vuota, nient'altro sostituisce una
directory e nessuna directory viene spostata nel proprio sottoalbero.

Una destinazione esistente è sovrascritta per impostazione
predefinita, saltata con `-n` e richiesta sul flusso di errore
standard con `-i` (una domanda rifiutata salta quello spostamento
senza errore; una risposta illeggibile non vale mai come consenso).
Il primo fallimento ferma l'esecuzione prima di ogni operando
successivo. `--` termina l'analisi delle opzioni: ogni argomento
successivo è un percorso.

La copia conserva i buchi di ogni file, i suoi attributi estesi — un
tipo di file RISC OS, un tipo e un creatore Mac —, i permessi e il
proprietario ovunque la destinazione li mantenga, e sposta un
collegamento simbolico come un collegamento con la stessa destinazione,
mai ciò a cui punta. Un file che chi lo sposta non può restituire al suo
proprietario diventa suo, senza i bit set-ID. Le marche temporali non
vengono conservate: nessuna chiamata di questo sistema le imposta. Gli
attributi che la destinazione non può mantenere vengono segnalati in un
unico avviso al termine dell'esecuzione, e in un record `omission` sul
flusso di informazioni standard, prima che le sorgenti scompaiano per
sempre.

## OPTIONS

- `-f, --force` — non chiedere mai prima di sovrascrivere. Vale l'ultimo
  tra `-f`, `-i` e `-n`.
- `-i, --interactive` — chiedere prima di sovrascrivere una
  destinazione esistente; acconsente solo una risposta che inizia con
  `y`/`Y`.
- `-n, --no-clobber` — non sovrascrivere mai una destinazione
  esistente.
- `-v, --verbose` — riportare ogni spostamento come
  `renamed 'source' -> 'dest'`, e uno spostamento tra volumi come
  `copied 'source' -> 'dest'` e poi `removed 'source'`.
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
  destinazione non esiste già.

## EXIT STATUS

- `0` — ogni spostamento è riuscito (un salto per `-n` e una domanda
  `-i` rifiutata non sono fallimenti).
- `1` — un guasto del file system, della domanda o dell'output; il
  motivo è stampato sul flusso di errore standard.
- `2` — la riga di comando non è stata compresa.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
