## NAME

chmod — cambiare i bit di modo di un file

## SYNOPSIS

`chmod [-cfRv] [--] MODE file...`

## DESCRIPTION

Cambia i bit di permesso di ogni operando file in `MODE`, in ordine.
`MODE` è un valore ottale assoluto (`644`, `0755`, …) che sostituisce
completamente i bit di permesso, oppure un elenco di clausole
simboliche separate da virgole `[ugoa]*([-+=]([rwxXst]*|[ugo]))+` o
`[-+=][0-7]+` (`g+w`, `o-rx`, `a=rx`, `u+s`, `g=u`, `=644`) che
trasformano i bit attuali del file. La `X` simbolica concede
l'esecuzione solo a una directory o a un file che porti già un bit di
esecuzione, `g=u` dà al gruppo i bit del proprietario, `s` imposta i
bit set-ID del proprietario e del gruppo, e `t` imposta lo sticky bit.
Una clausola che non nomina nessuno (`+x`, `=r`) cambia ogni campo
tranne i bit che la umask contiene.

Su una directory, i bit set-user-ID e set-group-ID restano come sono a
meno che il modo non li nomini: con `s` (`g-s`), con cifre ottali dopo
un operatore (`=755`) o con un valore ottale di cinque o più cifre
(`00755`). È il bit set-group-ID di una directory a dare alle sue nuove
voci il gruppo della directory, quindi `chmod 755` lo conserva.

Solo il proprietario di un file può cambiarne il modo; il kernel
rifiuta chiunque altro, e possedere una capability non concede alcuna
deroga. Un collegamento simbolico indicato come operando cambia il
file che nomina. Con `-R` un operando directory viene cambiato e poi
il suo contenuto viene cambiato ricorsivamente; un collegamento
simbolico incontrato dentro l'albero viene lasciato intatto, e così
anche ciò che nomina. Un file che non può essere raggiunto o cambiato
viene segnalato e gli altri vengono cambiati comunque. `--` termina
l'analisi delle opzioni: ogni argomento successivo è un operando. Per
un modo che inizia con `-`, scrivetelo senza il trattino (`a-w`) o
terminate prima le opzioni (`chmod -- -w file`).

## OPTIONS

- `-R, --recursive` — cambiare file e directory ricorsivamente.
- `-c, --changes` — segnalare solo i file il cui modo è davvero
  cambiato.
- `-v, --verbose` — segnalare ogni file elaborato, compreso quello
  il cui cambio è stato rifiutato o che non si è potuto raggiungere.
- `-f, --silent, --quiet` — sopprimere la maggior parte dei messaggi
  di errore; l'esecuzione fallisce comunque e lo stato di uscita lo
  segnala.
- `--preserve-root` — con `-R`, rifiutare di cambiare `/`, comunque lo
  si raggiunga.
- `--no-preserve-root` — trattare `/` come qualsiasi altra directory (il
  comportamento predefinito).
- `-h, -?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `chmod 644 notes.txt` — lettura/scrittura per il proprietario,
  sola lettura per gli altri.
- `chmod g+w shared.txt` — aggiungere la scrittura di gruppo ai bit
  attuali.
- `chmod -R a=rx Docs` — rendere l'albero `Docs` leggibile e
  attraversabile da tutti.
- `chmod +x build.sh` — rendere uno script eseguibile da tutti coloro
  che la umask consente.

## EXIT STATUS

- `0` — ogni cambio di modo è riuscito.
- `1` — un file non si è potuto raggiungere o cambiare, o l'output è
  fallito; ogni ragione è stampata sullo standard error (soppressa
  sotto `-f`). Anche una riga di comando non compresa, o il cui
  operando di modo non era né ottale né simbolico, termina con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47
  come `it-IT`).

## SEE ALSO

- `ls`
- `mkdir`
- `rm`
