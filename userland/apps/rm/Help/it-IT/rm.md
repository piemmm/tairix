## NAME

rm — rimuovere file e directory

## SYNOPSIS

`rm [-dfiIrRv] [--] file...`

## DESCRIPTION

Rimuove ogni operando file, in ordine. Un operando che non è una
directory è scollegato — viene rimosso il collegamento simbolico stesso,
mai ciò che nomina; un operando directory è rimosso solo con `-r` (che
rimuove il contenuto in profondità prima e poi la directory stessa) o,
quando è vuota, con `-d`. In entrambi i casi un operando il cui ultimo
componente è `.` o `..` è rifiutato, come richiede POSIX.

Con `-f` un operando inesistente è saltato in silenzio e nessuna
domanda è mai posta. `-i` chiede sul flusso di errore standard prima
di ogni rimozione, nominando ciò che rimuoverebbe
(`remove regular empty file 'a'?`, `remove symbolic link 'l'?`), e
prima di scendere in una directory che contiene qualcosa; `-I` chiede
una sola volta prima di rimuovere più di tre operandi o prima di una
rimozione ricorsiva. Una domanda rifiutata mantiene l'oggetto (o
salta l'intera esecuzione, con `-I`) senza errore; una risposta
illeggibile non vale mai come consenso. Vince l'ultimo tra `-f`, `-i`
e `-I`.

Senza `-f`, quando lo standard input è un terminale, prima di rimuovere
un file che il chiamante non può scrivere viene posta una domanda
(`remove write-protected regular file 'x'?`), e il file è rimosso solo
con una risposta che inizia con `y` o `Y`; per un collegamento simbolico
non viene mai posta alcuna domanda. Con `-i` la domanda nomina anche la
protezione.

Un percorso che non può essere ispezionato, letto o rimosso viene
segnalato e gli altri vengono rimossi comunque; una directory
rimasta con qualcosa dentro viene mantenuta senza dire nulla. Una
rimozione ricorsiva di `/`, comunque sia scritta, è rifiutata sotto
`--preserve-root`, il comportamento predefinito. `--` termina
l'analisi delle opzioni: ogni argomento successivo è un percorso.

## OPTIONS

- `-r, -R, --recursive` — rimuovere le directory e il loro contenuto.
- `-f, --force` — ignorare gli operandi inesistenti; non chiedere
  mai.
- `-d, --dir` — rimuovere le directory vuote.
- `-i, --interactive` — chiedere prima di ogni rimozione; acconsente
  solo una risposta che inizia con `y`/`Y`.
- `-I` — chiedere una sola volta prima di rimuovere più di tre
  operandi, o prima di una rimozione ricorsiva.
- `-v, --verbose` — riferire ogni rimozione come `removed 'file'`.
- `--preserve-root` — rifiutare di rimuovere `/` ricorsivamente (il
  comportamento predefinito).
- `--no-preserve-root` — permettere la rimozione di `/`. È accettata
  solo se scritta per esteso.
- `-h, -?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `rm notes.txt` — rimuovere un file.
- `rm -r Scratch` — rimuovere l'albero `Scratch` e tutto il suo
  contenuto.
- `rm -I a b c d` — chiedere una volta e, con `y`, rimuovere tutti e
  quattro i file.

## EXIT STATUS

- `0` — ogni rimozione è riuscita (una domanda rifiutata e un salto
  per `-f` non sono fallimenti).
- `1` — un percorso non si è potuto rimuovere, o la domanda o
  l'output non sono riusciti; ogni motivo è stampato sul flusso di
  errore standard. Anche una riga di comando non compresa termina
  con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

## SEE ALSO

- `cp`
- `ls`
- `mv`
