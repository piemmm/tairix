## NAME

settings — configurare la scrivania e questa macchina

## SYNOPSIS

`settings`

## DESCRIPTION

Apre una finestra della scrivania che elenca ogni categoria di impostazioni di
questo sistema: che cos'è la macchina, l'aspetto della scrivania, lo schermo,
la rete, i dispositivi con cui viene guidata, gli account che la usano e i
volumi che contiene. Scegliere una categoria nella barra laterale mostra il
suo pannello. Una categoria che contiene più pannelli apre e chiude invece il
loro elenco sul posto, e qualsiasi numero di quegli elenchi può restare aperto
contemporaneamente.

Settings non detiene alcuna autorità propria. Ogni modifica è o una richiesta
alla sessione della scrivania, cui appartengono le impostazioni dell'utente, o
un'esecuzione riautenticata del comando che già scrive quell'archivio: nulla
qui può elevare un privilegio.

Una categoria che questo sistema non può servire lo dice chiaramente e nomina
ciò che dovrebbe esistere perché possa farlo. Un controllo che non cambierebbe
nulla non viene mai mostrato.

Digitare nel campo di ricerca sopra la barra laterale per filtrarla alle
categorie e alle impostazioni che una parola raggiunge. `Tab` e `Shift+Tab`
spostano il fuoco tra campo di ricerca, percorso, barra laterale e pannello;
`Up` e `Down` percorrono la barra laterale e `Enter` apre la riga. `Right` e
`Left` aprono e chiudono l'elenco della categoria sotto il cursore. Una finestra
troppo stretta abbandona la barra laterale, e la prima briciola del percorso
elenca allora le categorie.

Si avvia dalla riga *Settings…* del menu di sistema della scrivania, o da una
shell tramite il suo percorso, `/System/Components/settings.app`. Richiede
una sessione grafica in esecuzione: senza di essa il canale della finestra è
irraggiungibile e il programma segnala il rifiuto sul flusso di errore
standard e termina.

Suono mostra ogni uscita e ingresso con la sua scelta predefinita, il livello
e il silenziamento, e ciò che sta registrando: le proprie registrazioni per
nome e quante ne fanno gli altri. I controlli cambiano subito il dispositivo,
e solo finché la propria sessione occupa la stanza che serve; un dispositivo
della stanza di un'altra sessione è mostrato ma qui non si può cambiare. La
scrivania ricorda ciò che si imposta. Cambiare il volume del dispositivo da cui
esce il suono, qui o dalla barra delle icone, riproduce un breve suono al nuovo
volume — dove un trascinamento si ferma, a ogni passo dei tasti freccia e
togliendo il silenziamento — così da sentire quanto è forte ora. In
*Sound Effects*, *Play feedback when the volume changes* lo disattiva. Non
segue gli altri suoni della scrivania: suona al volume del dispositivo stesso,
che siano attivi o no.

**Raspberry Pi** compare solo quando la macchina lo è. Imposta la ventola del
processore, comandata da una linea del connettore a 40 pin: *Off*, *On* sempre
oppure *Automatic*, che accende la ventola quando il processore raggiunge la
temperatura scelta e la spegne solo dopo che si è raffreddato del margine
scelto, così che non si accenda e si spenga attorno a un solo grado. La linea di
controllo si sceglie per numero GPIO e pin, insieme al livello, alto o basso, a
cui la ventola gira; una linea già usata da altro viene nominata anziché
offerta. Una linea del connettore non può alimentare da sola una ventola, che va
quindi collegata tramite un transistor. Il pannello mostra anche la temperatura
del processore e se la ventola sta girando. Quando *Automatic* non riesce a
leggere la temperatura, la ventola gira finché non ci riesce. Come le altre
impostazioni della macchina, una modifica attende *Apply*, che chiede un account
autorizzato a cambiarle.

**Storage** elenca ogni volume montato tranne quelli che un altro utente tiene
privati: dove è montato, il suo filesystem, il suo dispositivo e il suo
supporto, quanto è pieno e se è in buono stato. È un rapporto, non un
controllo — montare e smontare spettano al gestore di file e al comando
`mount` — e un volume il cui formato non ha una capacità fissa lo dice
anziché mostrare una barra. `df` riporta le stesse cifre in una shell.

**Files** imposta il modo in cui funziona il gestore di file: dove si apre una
nuova finestra e come mostra e ordina ciò che elenca, se mostra la colonna dei
luoghi e la barra degli strumenti, se le cartelle restano in cima, quali
estensioni dei nomi e quali voci sono nascoste, se un'immagine mostra il
proprio contenuto e una cartella ciò che contiene, se spostare delle voci nel
Cestino chiede prima conferma e se un'immagine disco o un archivio aperto viene
smontato una volta chiuse le sue finestre. Le estensioni dei nomi che nasconde
sono nascoste anche sul desktop. *Show on the desktop* ha una casella per ogni
tipo di unità — *The system drive*, *Fixed drives*, *Removable drives*,
*Network shares* e *Disk images and archives* — e il desktop mostra un'icona
per ogni unità collegata di un tipo spuntato, che la apre come fa il gestore di
file; all'inizio sono spuntate solo le unità rimovibili. Una riga ha effetto non
appena viene
scelta: dove si apre una nuova finestra e se mostra la colonna dei luoghi e la
barra degli strumenti valgono per la prossima finestra che viene aperta, e tutto
il resto raggiunge il desktop e tutte le finestre del gestore di file
contemporaneamente,
anche se una finestra mantiene una visualizzazione o un ordine scelti
personalmente in essa. *Settings…* nel menu del gestore di file stesso apre
questo pannello.

## EXIT STATUS

Zero dopo una chiusura pulita; diverso da zero quando il canale della finestra
o la regione di fotogrammi condivisa è stata rifiutata (il motivo è indicato
sul flusso di errore standard).
