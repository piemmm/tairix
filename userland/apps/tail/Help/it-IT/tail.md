## NAME

tail — mostrare l'ultima parte dei file

## SYNOPSIS

`tail [option...] [file...]`

## DESCRIPTION

Stampa le ultime 10 righe di ogni `file` sullo standard output. Con più
di un `file`, ogni parte è preceduta da un'intestazione `==> file <==`.
Senza `file`, o quando `file` è `-`, viene letto lo standard input.

`-n` e `-c` cambiano quanto viene stampato: un conteggio semplice (o
scritto con un `-` iniziale) stampa le ultime `num` righe o byte; un
conteggio scritto con un `+` iniziale stampa tutto **a partire** dalla
riga o dal byte `num` (contando da 1) fino alla fine. Un conteggio può
portare un suffisso moltiplicatore: `b` (512), `kB` (1000), `K` (1024),
`MB`, `M`, `GB`, `G`, e così via per `T`, `P`, `E`, `Z`, `Y`, `R`, `Q`
(una lettera sola moltiplica per potenze di 1024; con `B` per potenze di
1000; con `iB` per potenze di 1024).

La forma storica come primo argomento `tail -num` / `tail +num` (con una
lettera finale `b`/`c`/`l` facoltativa) è accettata, come nello strumento
GNU.

La modalità di inseguimento tiene ogni file aperto e stampa i nuovi dati
man mano che il file cresce. `-f` segue il file aperto tramite il
descrittore — dopo una rinomina o uno spostamento continua a seguire lo
stesso file. `-F` segue il *nome*: quando il file viene ruotato
(rinominato altrove e sostituito da un nuovo file al suo posto) riapre il
nuovo file, e `--retry` continua a provare un nome che non c'è ancora.
Un inseguimento si blocca finché il file non cambia davvero — non fa mai
girare la CPU a vuoto. `--pid=PID` termina l'inseguimento non appena il
processo `PID` esce, verificato ogni `--sleep-interval` (secondi,
predefinito 1). `--max-unchanged-stats=N` stabilisce quanti cicli fermi
passano prima che `-F` ricontrolli il nome in cerca di una rotazione
(predefinito 5). Un troncamento (il file si riduce) viene segnalato e il
file viene inseguito di nuovo dal suo nuovo inizio.

Quando del contenuto iniziale non viene mostrato, un record informativo
è scritto sul flusso di informazioni standard (fd 3); non cambia mai
l'output né lo stato di uscita. Un file non leggibile è segnalato sullo
standard error e l'esecuzione continua con il file successivo.

Su un terminale, ogni nome che compare nell'output viene messo tra
virgolette dove una shell ne avrebbe bisogno, e ogni carattere di
controllo o altro carattere invisibile al suo interno viene scritto in
forma di escape, così che un nome non possa comandare né riordinare lo
schermo; GNU lo mostra invariato. Una pipe o un file riceve il nome byte
per byte.

## OPTIONS

- `-c, --bytes <num>` — stampare gli ultimi `num` byte di ogni file; con
  un `+` iniziale, tutto dal byte `num` in poi.
- `-n, --lines <num>` — stampare le ultime `num` righe di ogni file; con
  un `+` iniziale, tutto dalla riga `num` in poi.
- `-q, --quiet, --silent` — non stampare mai le intestazioni
  `==> file <==`.
- `-v, --verbose` — stampare sempre le intestazioni `==> file <==`.
- `-z, --zero-terminated` — le righe sono delimitate da NUL invece che
  dall'a capo.
- `-f, --follow[=descriptor]` — tenere aperto il file e stampare i dati
  man mano che vengono aggiunti, seguendolo per descrittore.
- `-F` — seguire per nome (`--follow=name --retry`): riaprire il file
  quando viene ruotato.
- `--follow=name` — seguire il nome anziché il descrittore.
- `--retry` — continuare a provare ad aprire un file non ancora
  accessibile.
- `--pid <PID>` — durante un inseguimento, fermarsi non appena il
  processo `PID` muore.
- `--sleep-interval <N>` — secondi tra i ricontrolli di `--pid`/rotazione
  (frazioni ammesse; predefinito 1).
- `--max-unchanged-stats <N>` — cicli fermi prima che `-F` ricontrolli il
  nome in cerca di una rotazione (predefinito 5).
- `-h, -?` — mostrare l'aiuto breve di questo comando.

## EXAMPLES

- `tail log.txt` — stampare le ultime 10 righe di `log.txt`.
- `tail -n 3 a b` — stampare le ultime 3 righe di `a` e di `b`, ciascuna
  sotto la sua intestazione.
- `tail -c 1K image` — stampare gli ultimi 1024 byte di `image`.
- `tail -n +5 notes` — stampare `notes` dalla sua 5ª riga alla fine.
- `tail -f log.txt` — stampare le ultime 10 righe, poi le nuove righe
  man mano che vengono scritte.
- `tail -F /System/Logs/system` — seguire il registro di sistema
  attraverso le rotazioni.
- `tail -f --pid 1234 out` — seguire `out` finché il processo 1234 non
  esce.

## EXIT STATUS

- `0` — ogni file è stato stampato (o l'aiuto breve è stato scritto).
- `1` — un file non ha potuto essere letto, o l'output non ha potuto
  essere consegnato. Anche una riga di comando non compresa termina con
  `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per l'aiuto breve (un'etichetta BCP-47
  come `fr-FR`).

## SEE ALSO

- `head`
- `cat`
- `wc`
- `man`
