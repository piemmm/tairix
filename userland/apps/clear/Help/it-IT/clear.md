## NAME

clear — cancellare lo schermo del terminale

## SYNOPSIS

`clear [-x]`

## DESCRIPTION

Scrive la sequenza che porta il cursore nell'angolo in alto a sinistra
e cancella l'intero schermo, lasciandolo vuoto. La sequenza emessa
dipende dal terminale indicato in `TERM`; un terminale che non può
cancellare (un `TERM` sconosciuto degrada al profilo minimo) fa fallire
il comando invece di stampare byte che il terminale mostrerebbe come
caratteri spuri.

Dove il terminale lo consente, vengono cancellate anche le righe che
conserva sopra lo schermo — la cronologia di scorrimento — così che
nulla di ciò che era sullo schermo possa essere ritrovato scorrendo
indietro.

## OPTIONS

- `-x` — conservare la cronologia: cancellare solo lo schermo.
- `-h, -?` — mostrare la guida breve di questo comando.

## EXAMPLES

- `clear` — cancellare lo schermo.

## EXIT STATUS

- `0` — la sequenza di cancellazione è stata scritta.
- `1` — il terminale non può cancellare, o l'output non è stato
  consegnato. Anche una riga di comando non compresa termina con `1`.

## ENVIRONMENT

- `TERM` — il terminale la cui sequenza di cancellazione viene scritta.
- `LANG` — la locale preferita per la guida breve (un'etichetta BCP-47
  come `it-IT`).

## SEE ALSO

- `reset`
- `man`
