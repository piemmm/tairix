## NAME

files — browser grafico del filesystem

## SYNOPSIS

`files [--desktop] [directory] [-h | -?]`

## DESCRIPTION

Apre una finestra del desktop che elenca il file system, a partire dalla
`directory` indicata sulla riga di comando o, se non ne è indicata alcuna,
dalla cartella `UserFiles` dell'utente che lo avvia (la sua directory home
se quella non può essere elencata). Il titolo della finestra indica la
directory corrente; la finestra ne elenca le voci, ogni voce selezionata
evidenziata con il colore d'accento del tema attivo. Ogni lettura di una
directory è un normale elenco soggetto ai permessi, con l'identità
dell'utente che lo avvia: una directory illeggibile viene rifiutata, mai
indovinata.

Il desktop avvia il browser per voi e lo tiene sulla barra delle icone: il
menu della sua posizione elenca i vostri luoghi e tutto ciò che è montato, e
sceglierne uno vi apre una finestra. Un clic sulla posizione ne apre una
nella cartella `UserFiles`. Chiedere una cartella che ha già una finestra
porta quella finestra in primo piano anziché aprirne un'altra. Questa
istanza non ha la voce *Esci*: fa parte del desktop, e chiuderne le finestre
semplicemente la mette via.

Avviato per nome da una shell (o aperto su una cartella dal desktop) è
invece una normale applicazione: una finestra, che termina quando la
chiudete. In ogni caso richiede una sessione grafica attiva: senza, il
canale delle finestre è irraggiungibile, e il browser segnala il rifiuto
sullo standard error e termina.

La finestra si comanda da tastiera: `Giù` e `Su` spostano la selezione,
`Invio` apre la directory selezionata e `Backspace` risale alla directory
superiore. `F5` rilegge sia l'elenco sia la colonna dei luoghi; un volume
appena collegato vi compare da solo. `Ctrl+Shift+N` crea una nuova cartella.

Un elenco si apre senza nulla di selezionato. Un clic seleziona una voce, un
clic con `Ctrl` ne aggiunge o ne toglie una, e un clic con `Shift` seleziona
la serie a partire dall'ultima voce scelta; un clic su uno spazio vuoto
annulla la selezione. Trascinare su uno spazio vuoto traccia un riquadro che
seleziona tutto ciò che tocca mentre cresce; tenuto al bordo superiore o
inferiore dell'elenco, questo scorre, e `Escape` annulla ciò che il riquadro
ha selezionato.

Lo spazio intorno a ogni icona e al suo nome conta come vuoto, quindi un
riquadro può partire ovunque tra le voci. `Ctrl+A` seleziona tutto l'elenco
e `Ctrl+Shift+A` annulla la selezione; entrambi sono anche nel menu
contestuale.

Il nome di una voce è mostrato per intero, su due righe quando servono; un
nome troppo lungo anche per quelle conserva l'inizio e la fine, con `…` nel
mezzo, così l'estensione resta sempre visibile.

`F2` rinomina sul posto la voce selezionata, come fa un clic sul nome
dell'unica voce selezionata seguito da una pausa: il nome si apre per la
modifica non appena il clic non può più diventare un doppio clic. La parte
prima dell'estensione è selezionata, quindi ciò che si digita sostituisce il
nome e mantiene l'estensione. `Invio`, o un clic fuori dal nome, conferma il
nuovo nome — un nome che il volume rifiuta resta aperto con il motivo — e
`Escape` lo abbandona.

Trascinare le voci selezionate su un'altra finestra del gestore di file, su
una cartella in essa o sul desktop le copia lì; tenendo premuto Maiusc,
invece, le sposta. Il cursore mostra un più finché il rilascio copierebbe e
una freccia finché sposterebbe, e la cartella in cui finirebbe il rilascio è
evidenziata. Un singolo file trascinato sulla posizione di un'applicazione
nella barra delle icone vi viene aperto.

Il sottomenu *Nuovo* del menu contestuale crea una cartella, o un documento
vuoto di ogni tipo che un editor installato sa scrivere, e ne apre il nome
per la modifica.

`Alt+Enter` apre una finestra *Proprietà* sulla voce selezionata, come fa la
voce *Proprietà* del menu contestuale. È una finestra a sé, per cui se ne
possono aprire diverse insieme e l'elenco resta utilizzabile nel frattempo:
mostra che cosa è la voce, la sua dimensione, le sue date, dove punta un
alias, i suoi permessi e il suo proprietario, e gli attributi estesi che il
volume conserva per essa. Permessi, proprietario e attributi si possono
cambiare lì, ciascuno come una normale scrittura soggetta ai permessi con la
vostra identità: un rifiuto ne dice il motivo e non cambia nulla.
Riassegnare un proprietario richiede la capacità `CAP_FS_CHOWN`; una
sessione che non l'ha vede proprietario e gruppo segnati da un lucchetto,
con una riga che ne spiega il motivo.

`Sinistra` e `Destra` passano da una sezione all'altra della finestra. In
*Permessi*, `Giù` o `Tab` entra nei suoi controlli: le frecce passano
dall'uno all'altro, `Space` attiva o disattiva un permesso o apre il
proprietario o il gruppo per la modifica, e `Tab` o `Escape` torna alle
sezioni.

L'operando `directory` è trattato come input non fidato: deve essere un
percorso assoluto entro il limite di lunghezza dei percorsi del
sistema, e ognuno dei suoi componenti deve essere un vero nome di
directory — `.` e `..` non lo sono, così che una scrittura non possa
mai indicare un luogo diverso da come si legge. Una directory che
infrange una di quelle regole, o che l'utente che ha avviato il
programma non può elencare, viene rifiutata con il motivo sul flusso di
errore standard e la finestra si apre invece sulla cartella `UserFiles`,
così che un argomento sbagliato non lasci mai l'utente senza finestra.
Un secondo operando viene rifiutato del tutto anziché ignorato.

## OPTIONS

- `--desktop` — eseguire come componente gestore di file del desktop stesso:
  una posizione permanente sulla barra delle icone che offre i propri luoghi
  e i volumi montati, nessuna finestra finché non ne viene chiesta una, e
  nessun modo di uscire. La sessione desktop passa questa opzione all'avvio;
  indicare una `directory` accanto ad essa è rifiutato, perché un componente non
  apre alcuna finestra in cui metterla.
- `-h, -?` — mostrare la breve guida di questo comando e uscire.

## EXIT STATUS

Zero dopo una chiusura pulita, o dopo che è stata mostrata la breve
guida; `2` quando la riga di comando non è stata compresa; altrimenti
diverso da zero quando il canale finestra, la regione dei frame
condivisa o l'elenco iniziale della directory è stato rifiutato (il
motivo è indicato sul flusso di errore standard).
