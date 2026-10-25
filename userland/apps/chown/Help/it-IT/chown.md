## NAME

chown — cambiare il proprietario e il gruppo di un file

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

Cambia l'utente e il gruppo proprietari di ogni operando file, in
ordine. `OWNER` cambia l'utente e mantiene il gruppo, `OWNER:GROUP`
cambia entrambi e `:GROUP` cambia solo il gruppo. `OWNER:` dà inoltre al
file il gruppo primario del proprietario. Un operando vuoto, o `:` da
solo, non cambia né l'uno né l'altro. La vecchia grafia `OWNER.GROUP`
funziona con un avviso, a meno che l'intero operando non sia il nome di
un account. Con `--reference`, ogni file prende il proprietario e il
gruppo di ciò che `RFILE` nomina.

`OWNER` e `GROUP` sono nomi di account e di gruppi, oppure numeri: si
prova prima un nome, e un numero viene accettato solo quando niente ha
quel nome. Un `+` iniziale (`+1000`) indica sempre un numero.

Dare un file a un altro account, o a un gruppo di cui non siete membri,
richiede la capability `CAP_FS_CHOWN`; senza di essa potete cambiare il
gruppo di un file che possedete in uno dei vostri gruppi. Un cambio
azzera il bit set-user-ID di un file, e il bit set-group-ID quando il
suo gruppo può eseguirlo; una directory li conserva entrambi.

Un collegamento simbolico indicato come operando cambia il file che
nomina, o con `-h` il collegamento stesso. Con `-R` vengono cambiati un
operando directory e tutto ciò che contiene, ogni directory dopo il suo
contenuto. Un collegamento simbolico incontrato dentro l'albero viene
cambiato esso stesso, e ciò che nomina non viene mai toccato; con
`--dereference` resta intatto anche il collegamento. Con `-R` un
collegamento simbolico indicato come operando viene cambiato esso stesso
e non viene seguito: `--dereference` cambia invece ciò che nomina, e
`-H` percorre una directory che esso nomina. Un file che non può essere
raggiunto o cambiato viene segnalato e gli altri vengono cambiati
comunque. Nei resoconti ogni proprietario e gruppo è indicato per nome,
o per numero quando nessun account o gruppo ha quel numero. `--` termina
l'analisi delle opzioni: ogni argomento successivo è un operando.

## OPTIONS

- `-c, --changes` — segnalare solo i file la cui proprietà è davvero
  cambiata.
- `-f, --silent, --quiet` — sopprimere la maggior parte dei messaggi di
  errore; l'esecuzione fallisce comunque e lo stato di uscita lo
  segnala.
- `-v, --verbose` — segnalare ogni file elaborato, compreso quello il
  cui cambio è stato rifiutato o che non si è potuto raggiungere.
- `--dereference` — cambiare ciò che un collegamento simbolico nomina
  anziché il collegamento; senza `-R` è il comportamento predefinito.
- `-h, --no-dereference` — cambiare il collegamento simbolico stesso
  anziché ciò che nomina.
- `--preserve-root` — con `-R`, rifiutare di cambiare `/`, comunque lo
  si raggiunga.
- `--no-preserve-root` — trattare `/` come qualsiasi altra directory (il
  comportamento predefinito).
- `--from=CURRENT_OWNER:CURRENT_GROUP` — cambiare un file solo se il suo
  proprietario e il suo gruppo attuali sono questi; omettete l'uno o
  l'altro per accettarne uno qualsiasi.
- `--reference=RFILE` — dare a ogni file il proprietario e il gruppo di
  `RFILE` invece di indicarli.
- `-R, --recursive` — cambiare file e directory ricorsivamente.
- `-H` — con `-R`, percorrere la directory a cui porta un collegamento
  simbolico indicato come operando.
- `-L` — con `-R`, seguire ogni collegamento simbolico a una directory;
  rifiutato, perché un cambio ricorsivo non segue mai un collegamento
  dentro l'albero.
- `-P` — con `-R`, non seguire alcun collegamento simbolico (il
  comportamento predefinito). Vince l'ultimo dato tra `-H`, `-L` e `-P`.
- `-?, --help` — mostrare la guida breve di questo comando.
- `--version` — stampare il nome e la versione dello strumento.

## EXAMPLES

- `chown alice notes.txt` — dare `notes.txt` ad `alice`, mantenendone il
  gruppo.
- `chown alice:staff notes.txt` — darlo ad `alice` e al gruppo `staff`.
- `chown :staff shared.txt` — cambiare solo il gruppo.
- `chown -R alice Projects` — dare l'albero `Projects` ad `alice`.
- `chown -R --from=bob alice Shared` — dare ad `alice` solo ciò che
  `bob` possiede in `Shared`.
- `chown --reference=template.txt notes.txt` — dare a `notes.txt` il
  proprietario e il gruppo di `template.txt`.

## EXIT STATUS

- `0` — ogni cambio è riuscito.
- `1` — un file non si è potuto raggiungere o cambiare, o l'output è
  fallito; ogni ragione è stampata sullo standard error (soppressa sotto
  `-f`). Anche una riga di comando non compresa, o un proprietario, un
  gruppo o un file di riferimento che non si è potuto leggere, termina
  con `1` e non cambia nulla.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

## SEE ALSO

- `chmod`
- `ls`
- `stat`
