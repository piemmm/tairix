## NAME

music — il lettore musicale del desktop

## SYNOPSIS

`music`

## DESCRIPTION

Riproduce un elenco di brani in una finestra. Aprite file o un'intera
cartella con il selettore di file del desktop, oppure aprite un brano dal
gestore di file: si aggiunge all'elenco del lettore già aperto. I brani con la
stessa frequenza, lo stesso formato di campione e la stessa disposizione dei
canali si susseguono senza pause.

Il lettore non detiene alcuna capacità sul file system. La sessione del
desktop sfoglia per suo conto e gli delega, una sola volta e in sola lettura,
esattamente i file che l'utente sceglie — per una cartella, i file che contiene
e che questo lettore apre. Nessun file viene decodificato dentro il lettore: il
suono e la copertina sono decodificati ciascuno da un processo separato privo
di qualsiasi accesso, così un file malformato od ostile non può raggiungere
nulla di ciò che il lettore raggiunge.

La parte alta della finestra mostra cosa suona: la copertina, il titolo,
l'artista e l'album, il formato, la posizione e un indicatore di livello per
canale. Sotto ci sono i controlli di riproduzione, la riproduzione casuale e la
ripetizione e il volume, e sotto ancora l'elenco. Trascinate il cursore della
posizione per spostarvi e quello del volume per impostare il livello;
entrambi agiscono dove li lasciate. Fate doppio clic su un brano per
riprodurlo. Una pressione secondaria sull'elenco apre il menu del lettore, che
sceglie anche l'uscita e se livellare i brani secondo l'intensità indicata dai
loro tag.

Un file che il lettore non riesce a leggere viene escluso, con il motivo sulla
riga di stato.

* `Space` — riprodurre o mettere in pausa
* `Enter` — riprodurre il brano selezionato
* `Left` / `Right` — dieci secondi indietro o avanti
* `Ctrl` + `Left` / `Right` — il brano precedente o il successivo
* `Up` / `Down` — selezionare il brano sopra o sotto
* `Alt` + `Up` / `Down` — spostare il brano selezionato
* `Delete` — togliere dall'elenco il brano selezionato
* `+` / `-` — tre decibel più forte o più piano
* `S` — riproduzione casuale o in ordine
* `R` — non ripetere nulla, ripetere l'elenco o il brano
* `Ctrl` + `O` — aprire file
* `Ctrl` + `Shift` + `O` — aprire una cartella

## OPTIONS

`-h`, `-?`, `--help`
: Scrivere questa guida sull'output standard ed uscire.

## EXIT STATUS

Zero dopo la chiusura della finestra o la scelta di Esci. Diverso da zero
quando il canale della finestra, la regione d'immagine condivisa o la sessione
del desktop è stato rifiutato; il motivo è indicato sullo standard error.
