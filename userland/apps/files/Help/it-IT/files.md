## NAME

files — browser grafico del filesystem

## SYNOPSIS

`files --desktop [--mount] [path] [-h | -?]`

## DESCRIPTION

Il gestore di file del desktop stesso, che elenca il file system in
finestre. Una finestra si apre nella cartella che gli viene chiesta — o, per
un file, nella cartella che contiene quel file, con il file selezionato — o,
se non ne è indicata alcuna, nella cartella scelta nelle impostazioni: la
cartella `Home` dell'utente che lo avvia, a meno che non abbia scelto la
sua cartella `Desktop` o le unità di archiviazione collegate (la sua
directory home se quella non può essere elencata). Il titolo della
finestra indica la directory corrente; la finestra ne elenca le voci, ogni
voce selezionata evidenziata con il colore d'accento del tema attivo. Ogni
lettura di una directory è un normale elenco soggetto ai permessi, con
l'identità dell'utente che lo avvia: una directory illeggibile viene
rifiutata, mai indovinata.

Il desktop avvia il gestore di file insieme alla sessione e lo tiene sulla
barra delle icone, e questo non esce mai: il menu della sua posizione elenca
i vostri luoghi e tutto ciò che è montato, e sceglierne uno vi apre una
finestra; *Rete* segue, e *Cestino* è l'ultima voce: apre
il vostro Cestino, ed è in grigio e ne dice il motivo quando non c'è una
directory home in cui tenerne uno. Un volume che potete
espellere — una chiavetta USB, una scheda di memoria o un altro disco
rimovibile, oppure un'immagine disco o una condivisione che avete montato —
apre un sottomenu che contiene *Espelli*, e quello di un volume di cui
potete cambiare il titolo — un'immagine disco vostra, o un disco il cui
formato conserva un titolo — contiene *Titolo*: questa voce apre un campo con
il titolo, pronto da modificare, e premere `Return` scrive il nuovo sul
disco; un titolo vuoto lo cancella. Quando non potete cambiare voi stessi il
titolo di un disco, il desktop chiede il nome e la password di un
amministratore, e lo cambia con il suo consenso. Un clic sulla posizione ne apre una nella cartella scelta
nelle impostazioni. Chiedere una
cartella che ha già una finestra porta quella finestra in primo piano anziché
aprirne un'altra. Il menu non ha la voce *Esci*: il gestore di file fa parte
del desktop, chiuderne le finestre semplicemente lo mette via e, se mai
dovesse fermarsi, il desktop lo riavvia.

*Rete*, nel menu della posizione subito sopra *Cestino*, apre una
finestra sui computer della rete locale che condividono file. Ognuno è
elencato con il suo nome, l'indirizzo a cui lo si raggiunge, il suo gruppo
di lavoro o dominio e che cosa è, e l'elenco si riempie man mano che i
computer rispondono. La rete viene esplorata solo finché una finestra la
mostra. Un computer che non risponde più appare in grigio finché non viene
dimenticato; un nome che mescola alfabeti come serve a imitare un altro nome
porta un segnale di avvertimento al posto della sua immagine; e quando un
computer offre più di qualche voce, le altre sono contate in una riga a sé
anziché scalzare le restanti. I nomi in rete li sceglie chi gestisce ciascun
computer, quindi un nome non prova mai quale computer state guardando. La
connessione alle condivisioni di un computer non è disponibile su questo
sistema, e aprirne uno lo dice.

Funziona solo come lo avvia il desktop: avviato in qualunque altro modo — da
una shell tramite il suo percorso, `/System/Components/files.app` — rifiuta
l'avvio, indicandone il motivo sullo standard error. Richiede una sessione
grafica attiva: senza, il canale delle finestre è irraggiungibile, e segnala
il rifiuto sullo standard error e termina.

La finestra si comanda da tastiera: le frecce `Down` e `Up` spostano la
selezione, `Enter` apre la directory selezionata e `Backspace` risale alla
directory superiore. `F5` rilegge sia l'elenco sia la colonna dei luoghi;
un volume appena collegato vi compare da solo. `Ctrl+Shift+N` crea una
nuova cartella.

Un elenco si apre senza nulla di selezionato. Un clic seleziona una voce, un
clic con `Ctrl` ne aggiunge o ne toglie una, e un clic con `Shift` seleziona
la serie a partire dall'ultima voce scelta; un clic su uno spazio vuoto
annulla la selezione. Un doppio clic apre una voce, e quella che apre
altrove — un documento nella sua applicazione, un'applicazione, un'immagine
disco come volume — viene lasciata deselezionata una volta aperta, mentre
quella che non si è potuta aprire resta selezionata. Tenendo premuto `Shift`,
elenca il contenuto di un'applicazione anziché avviarla. `Enter` apre la
selezione e la mantiene. Trascinare su uno spazio vuoto traccia
un riquadro che seleziona tutto ciò che tocca mentre cresce; tenuto al bordo
superiore o inferiore dell'elenco, questo scorre, e `Escape` annulla ciò che
il riquadro ha selezionato.

Lo spazio intorno a ogni icona e al suo nome conta come vuoto, quindi un
riquadro può partire ovunque tra le voci. `Ctrl+A` seleziona tutto l'elenco
e `Ctrl+Shift+A` annulla la selezione; entrambi sono anche nel menu
contestuale.

Il nome di una voce è mostrato per intero, su due righe quando servono; un
nome troppo lungo anche per quelle conserva l'inizio e la fine, con `…` nel
mezzo, così l'estensione resta sempre visibile. Un'applicazione è mostrata
senza la sua estensione `.app`, e le impostazioni possono mostrarla, oppure
nascondere anche le estensioni dei tipi di file che il sistema conosce.
Un'estensione resta visibile laddove nasconderla lascerebbe un nome che si
legge come un altro tipo di file o un'altra applicazione — `invoice.pdf.app`
è mostrato per intero — e una finestra ordina le sue voci in base ai nomi
così come sono mostrati. Le voci il cui nome inizia con un punto sono
elencate solo quando le impostazioni lo chiedono.

`F2` rinomina sul posto la voce selezionata, come fa un clic sul nome
dell'unica voce selezionata seguito da una pausa: il nome si apre per la
modifica non appena il clic non può più diventare un doppio clic. La parte
prima dell'estensione è selezionata, quindi ciò che si digita sostituisce il
nome e mantiene l'estensione; anche un'estensione che la finestra non mostra
viene mantenuta. `Enter`, o un clic fuori dal nome, conferma il nuovo nome —
un nome che il volume rifiuta resta aperto con il motivo — e `Escape` lo
abbandona. Un clic che ha rinominato la voce non fa nient'altro, perché la
cartella potrebbe essersi riordinata sotto il puntatore.

Trascinare le voci selezionate su un'altra finestra del gestore di file, su
una cartella in essa o sul desktop le copia lì; tenendo premuto `Shift`,
invece, le sposta. Il cursore mostra un più finché il rilascio copierebbe e
una freccia finché sposterebbe, e la cartella in cui finirebbe il rilascio è
evidenziata. Un singolo file trascinato sulla posizione di un'applicazione
nella barra delle icone vi viene aperto. Un'unità trascinata fuori da
`/Storage` non viene mai copiata né spostata: lascia un collegamento dove
viene rilasciata, e il cursore mostra una freccia curva. Il collegamento apre
l'unità finché è collegata; quando non lo è, aprirlo lo dice, e sul desktop
viene disegnato sbiadito.
Copiare o spostare conserva i buchi di ogni file e i suoi attributi
estesi, come un tipo di file RISC OS; ogni attributo che la destinazione
non può conservare viene indicato in un avviso al termine dell'operazione.

Se lì c'è già una voce con lo stesso nome, la finestra propria dell'operazione
chiede cosa farne: *Sostituisci* mette la nuova al suo posto, *Salta* lascia
quella che c'è già e *Interrompi* termina l'operazione, conservando ciò che ha
già fatto. Una cartella che incontra una cartella con il suo nome offre
*Unisci* al posto di *Sostituisci*: ciò che contiene finisce nella cartella già
presente, e per ogni file al suo interno che c'è già viene chiesto a sua volta.
Spuntare *Fai lo stesso per ogni file che esiste già* dà la stessa risposta a
tutte le domande successive su un file. Quando una cartella incontra una
cartella, la casella dice *Fai lo stesso per ogni cartella che esiste già* e
risponde solo alle cartelle successive, così per ogni file di una cartella
unita viene ancora chiesto. `Escape` equivale a *Interrompi*, e `Enter` dà la
scelta su cui si trova la tastiera, all'inizio *Salta*; `Tab` passa dalla
casella alle scelte, e `Space` spunta la casella. Sul desktop, una voce
sostituita resta dove è stata rilasciata, mentre una saltata o unita rimane
dov'era.

Il menu contestuale si apre con *Visualizza*. Sotto, un sottomenu nomina ciò
su cui agisce il menu — *File «notes.txt»*, con un nome lungo accorciato nel
mezzo perché l'estensione resti visibile, o *Selezione* quando sono
selezionate più voci — e contiene *Apri*, *Apri con…*, *Monta in sola
lettura*, *Espelli*, *Ottieni informazioni…*, *Rinomina*, *Taglia*, *Copia*
ed *Elimina*; una voce che lì non può agire è in grigio e ne dice il motivo.
Seguono *Nuovo*, poi *Seleziona tutto* e *Annulla selezione*, e *Incolla*.
Nel Cestino, *Svuota Cestino* è l'ultima voce: una volta che confermate,
elimina definitivamente tutto ciò che si trova nel Cestino. Un clic destro su
uno spazio vuoto annulla la selezione, quindi
il sottomenu della voce è in grigio.

Aprire un'immagine disco o un archivio — un file `.iso`, `.adf`, `.hdf`,
`.zip` e simili — lo monta per voi in `/Storage`, scrivibile dove lo
consentono sia il file sia il suo formato, e ne apre la finestra;
*Monta in sola lettura* lo monta invece in sola lettura. Aprirne uno sul
desktop fa lo stesso. Quando nessuna finestra lo mostra più, viene di nuovo
smontato, così `/Storage` contiene solo ciò che è in uso; se qualcos'altro
lo sta ancora usando — per esempio un suo documento aperto in un'altra
applicazione — resta montato, e lo smontaggio viene ritentato non appena
quello potrebbe averlo rilasciato. Le impostazioni possono invece tenere
montato ciò che aprite, finché non lo espellete o non
termina la vostra sessione di accesso. *Espelli* toglie un volume che
avete montato, o un disco rimovibile mentre state usando il computer stesso,
dalla sua icona, dalla sua riga nella barra dei luoghi, il cui menu
contestuale contiene *Apri* ed *Espelli*, o dal menu della posizione sulla
barra delle icone; le vostre finestre su di esso passano prima a `/Storage`.
Un disco rimovibile su cui è ancora aperto un file non viene espulso, e
viene spiegato perché. Quando non avete il permesso di espellere un disco
rimovibile — non siete voi la persona davanti al computer — il desktop
chiede il nome e la password di un amministratore, e lo espelle con il suo
consenso. Un volume sparito — rimosso il suo
disco, o perso il suo server — prima che fosse salvato tutto ciò che
avevate scritto su di esso viene tolto dalla barra ma conserva la sua
icona in `/Storage`, e ciò che non era stato salvato va perso; *Espelli*
su quell'icona chiede conferma prima di lasciarlo andare.

Il sottomenu *Nuovo* del menu contestuale crea una cartella, o un documento
vuoto di ogni tipo che un editor installato sa scrivere, e ne apre il nome
per la modifica.

Il sottomenu *Visualizza* del menu contestuale mostra le voci come icone
grandi, l'impostazione predefinita; come icone piccole, ogni nome su una
riga accanto alla sua immagine; o come elenco, una riga per voce con
l'immagine, la dimensione, la data dell'ultima modifica e il tipo. Un'unità
elencata tra esse mostra invece lo spazio libero, il file system e quanto
bene funziona. Sotto le visualizzazioni, il sottomenu ordina la finestra
per nome, tipo, dimensione o data dell'ultima modifica — oppure, per un
elenco di unità, per nome, spazio libero, file system o stato — in ordine
crescente o decrescente. Anche un clic sul titolo di una colonna ordina
l'elenco in base a essa, e un secondo clic inverte l'ordine. Le cartelle
restano in cima qualunque ordine si scelga, a meno che le impostazioni non
le mettano tra i file.

`Ctrl+,` apre le impostazioni del browser nell'applicazione Impostazioni,
sotto *File*: dove si apre una nuova finestra, come le finestre mostrano e
ordinano le loro voci e se una nuova mostra la colonna dei luoghi e la
barra degli strumenti; se le cartelle restano in cima; quali estensioni dei
nomi e quali voci sono nascoste; se un'immagine mostra il proprio contenuto
e una cartella ciò che contiene; se spostare delle voci nel Cestino chiede
prima conferma; e se un'immagine disco o un archivio che avete aperto viene
smontato una volta chiuse le sue finestre. Una modifica si applica a tutte
le finestre insieme e viene ricordata; una finestra mantiene una
visualizzazione o un ordine che voi stessi avete scelto in essa.

`Alt+Enter` apre la finestra delle *informazioni* della voce selezionata, come
fa *Ottieni informazioni…* nel suo sottomenu del menu contestuale. È una finestra a sé, per cui se ne
possono aprire diverse insieme e l'elenco resta utilizzabile nel frattempo:
mostra che cosa è la voce, la sua dimensione, le sue date, dove punta un
alias, i suoi permessi e il suo proprietario, e gli attributi estesi che il
volume conserva per essa. Permessi, proprietario e attributi si possono
cambiare lì, ciascuno come una normale scrittura soggetta ai permessi con la
vostra identità: un rifiuto ne dice il motivo e non cambia nulla.
Riassegnare un proprietario richiede la capacità `CAP_FS_CHOWN`; una
sessione che non l'ha vede proprietario e gruppo segnati da un lucchetto,
con una riga che ne spiega il motivo. La finestra delle *informazioni* di
un'unità si apre sulla sua sezione *Unità*, il cui campo *Titolo* cambia il
titolo del volume allo stesso modo: fateci clic o premete `Tab`, scrivete il
nuovo titolo e premete `Return`; `Escape` lo rimette com'era.

Le frecce `Left` e `Right` passano da una sezione all'altra della
finestra. In *Permessi*, `Down` o `Tab` entra nei suoi controlli: le
frecce passano dall'uno all'altro, `Space` attiva o disattiva un permesso
o apre il proprietario o il gruppo per la modifica, e `Tab` o `Escape`
torna alle sezioni.

L'operando `path` è trattato come input non fidato: deve essere un
percorso assoluto entro il limite di lunghezza dei percorsi del sistema,
e ognuno dei suoi componenti deve essere un vero nome — `.` e `..` non
lo sono, così che una scrittura non possa mai indicare un luogo diverso
da come si legge. Un percorso che infrange una di quelle regole, o che
l'utente che ha avviato il programma non può elencare, viene rifiutato
con il motivo sul flusso di errore standard e la finestra si apre invece
sulla cartella `Home`, così che un argomento sbagliato non lasci mai
l'utente senza finestra. Un secondo operando viene rifiutato del tutto
anziché ignorato.

## OPTIONS

- `--desktop` — eseguire come gestore di file del desktop stesso: una
  posizione permanente sulla barra delle icone che offre i propri luoghi e i
  volumi montati, e nessun modo di uscire. La sessione desktop passa questa
  opzione ogni volta che avvia il gestore di file, che funziona solo con
  essa; un `path` indicato accanto ad essa vi apre una finestra.
- `--mount` — aprire `path` come immagine disco o archivio, allo stesso modo
  dell'apertura in una finestra: viene montato per voi e se ne apre la
  finestra, e viene smontato di nuovo una volta chiuse le sue finestre,
  secondo le impostazioni. Questa opzione richiede un `path`.
- `-h, -?` — mostrare la breve guida di questo comando e uscire.

## EXIT STATUS

Non termina finché il desktop è in esecuzione. Zero dopo che è stata mostrata
la breve guida; `2` quando la riga di comando non è stata compresa o non
indicava `--desktop`; altrimenti diverso da zero in caso di rifiuto del
canale finestra o della regione dei frame condivisa (il motivo è indicato sul
flusso di errore standard).
