## NAME

mkdir — creare directory

## SYNOPSIS

`mkdir [-pv] [-m MODE] [--] directory...`

## DESCRIPTION

Crea ogni directory indicata come operando, in ordine. Senza `-p` la
directory padre di ogni operando deve già esistere e l'operando stesso
non deve esistere; un operando che non può essere creato viene segnalato
e il successivo viene creato comunque.

Con `-p` viene creato prima ogni antenato mancante, dal più esterno al
più interno, e un operando (o antenato) che esiste già come directory
non è un errore. Un antenato che esiste come file fallisce comunque:
nulla viene mai sostituito in silenzio.

Una nuova directory prende il modo `a=rwx` meno la umask. Con `-m` ogni
operando directory creato riceve invece esattamente `MODE`, qualunque
sia la umask: un valore ottale o clausole simboliche, come li legge
`chmod`, a partire da `a=rwx`. Un antenato mancante che `-p` crea
conserva i permessi di scrittura e di ricerca del suo proprietario
comunque sia impostata la umask, così la directory sotto di esso può
sempre essere creata. Una directory creata dentro una directory
set-group-ID conserva il bit che eredita, a meno che `MODE` non lo
nomini. Un operando che esiste già conserva il proprio modo. `--`
termina l'analisi delle opzioni: ogni argomento successivo è un
percorso.

## OPTIONS

- `-m, --mode=MODE` — assegnare a ogni operando directory creato il modo
  `MODE`, ottale o simbolico come in `chmod`, invece di `a=rwx` meno la
  umask.
- `-p, --parents` — creare le directory padri mancanti; un operando che
  è già una directory non è un errore.
- `-v, --verbose` — segnalare ogni directory creata come
  `mkdir: created directory 'dir'`.
- `-h, -?` — mostrare la guida breve di questo comando (anche
  `--help`).

## EXAMPLES

- `mkdir Notes` — creare una directory nella directory corrente.
- `mkdir -p Projects/os/build` — creare l'intera catena, saltando le
  parti già esistenti.
- `mkdir -pv Home:/tools/bin` — creare sotto una radice alias,
  segnalando ogni nuova directory.
- `mkdir -m 700 Private` — creare una directory che solo il suo
  proprietario può usare.

## EXIT STATUS

- `0` — ogni directory è stata creata (o, con `-p`, esisteva già).
- `1` — un errore del filesystem o dell'output, o un modo che non si è
  potuto assegnare; la ragione è stampata sullo standard error. Anche
  una riga di comando non compresa, o un `MODE` che non è un modo,
  termina con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

## SEE ALSO

chmod, rmdir, rm, ls
