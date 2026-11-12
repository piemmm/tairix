## NAME

PCEmulator — emulatore di macchine IBM PC e di classe XT

## SYNOPSIS

`PCEmulator`

## DESCRIPTION

Esegue PC dell'epoca dall'8088 all'80186 in finestre del desktop: di ogni
macchina il processore, la scheda video, i dischi, il suono e le porte, alla
velocità che aveva la macchina.

La finestra della libreria mostra ogni macchina come un riquadro con il suo
ultimo schermo. Nuova crea una macchina da uno di quattro modelli — l'IBM PC,
l'IBM PC XT, un XT turbo e una workstation 80186 — con il nome richiesto e,
su un modello con un controller del disco rigido, con un disco rigido da
30 MB che avvia TAIRiX DOS. Avvia, o un doppio clic, avvia la macchina scelta
in una finestra tutta sua, o ne porta in primo piano la finestra; più
macchine funzionano insieme. Impostazioni cambia l'hardware di una macchina
mentre è spenta, Duplica copia una macchina con i suoi dischi, ed Elimina ne
elimina una con i suoi dischi una volta confermato. Al primo avvio PCEmulator
crea un XT turbo. Ciò che una macchina scrive sui suoi dischi, e l'ora a cui è
regolato il suo orologio, restano da un'esecuzione all'altra.

L'XT turbo e la workstation 80186 hanno un coprocessore numerico 8087, e
Impostazioni ne monta uno su qualsiasi macchina o lo toglie. Un programma che
attende il coprocessore senza prima accertarsi che ci sia si ferma su una
macchina che non lo ha, come accadeva sull'hardware reale.

L'immagine è quella del monitor, quattro unità di larghezza ogni tre di
altezza, e ogni punto della scheda video mantiene i bordi netti qualunque sia
la dimensione della finestra. Chiudere la finestra di una macchina la
sospende: riavviata, riprende da dove si era fermata, con i dischetti fuori
dalle unità e l'orologio avanzato del tempo in cui è rimasta via. L'hardware
di una macchina sospesa si cambia dopo averla avviata e spenta.

In fondo alla finestra di una macchina, ognuna delle sue unità ha una spia
che si accende mentre l'unità lavora. Premere lì un'unità a dischetti offre
Inserisci disco, che sceglie un'immagine di dischetto da mettere nell'unità,
ed Espelli. Un'immagine di dischetto trascinata sulla finestra va nell'unità
su cui viene rilasciata, o altrimenti nella prima, e lì non si può
scrivere.

All'estremità di quella barra, il menu della macchina la mette in pausa, la
reimposta, le invia Ctrl+Alt+Canc e fissa la velocità del suo processore per
la sessione. Visualizzazione attiva e disattiva le linee di scansione, il
bagliore, la persistenza del fosforo, il rumore e l'ondulazione dell'immagine,
e la macchina conserva ciò che è scelto. Cattura schermata salva lo schermo
della macchina come immagine PNG dove indica il pannello Salva. Schermo intero
riempie lo schermo con la finestra e lascia la barra al suo posto, dove lo
stesso menu lo restituisce. Spegni spegne la macchina e ne conserva solo
l'ultima schermata.

Copia il testo dello schermo mette negli appunti il testo che mostra lo schermo
della macchina, finché mostra del testo. Incolla digita il testo degli appunti
con la tastiera della macchina, ogni carattere dopo che il programma ha letto
il precedente. Ciò che la macchina non può digitare così com'è — i volti e le
frecce del PC, o una lettera che non conosce — viene tralasciato, e la finestra
lo dice. Smetti di incollare, o un tasto premuto nella finestra della macchina,
ferma la digitazione.

Condividi una cartella presta alla macchina una tua cartella, scelta nel
selettore, come un'unità a sé: la prima lettera che i suoi dischi lasciano
libera, una volta avviato TAIRiX DOS. Un nome che DOS non può contenere viene
accorciato, come Windows accorcia un nome lungo, e una cartella condivisa in
sola lettura, o un file che non puoi modificare, non può essere modificato
dalla macchina. Ogni cartella condivisa ha un posto sulla barra che ne nomina
l'unità; premerlo offre Interrompi condivisione. Una macchina condivide fino a
quattro cartelle, e nessuna una volta sospesa o spenta.

Debugger, nel menu della macchina, apre una finestra sulla macchina: i suoi
registri, il suo 8087, il suo codice e la sua memoria com'erano quando si è
fermata, e i suoi punti di interruzione. Interrompi ferma la macchina dove si
trova; Continua, Passo, Passo oltre e Passo fuori la fanno ripartire, come F5,
F11, F10 e Maiusc+F11, e F5 ferma anche una macchina in esecuzione. Premere
accanto a una riga di codice vi imposta un punto di interruzione o lo toglie,
come fa F9 per la riga corrente, e Aggiungi punto di interruzione ferma invece
la macchina quando scrive o legge la memoria, legge o scrive una porta, o
entra in un'interruzione. Il tempo della macchina resta fermo finché è ferma,
e chiudere il debugger la lascia proseguire.

Finché la finestra di una macchina detiene la tastiera, ogni tasto arriva alla
macchina come un tasto della sua tastiera, compresi Ctrl, Alt e i tasti di
blocco; il desktop si tiene Ctrl+Alt+Canc, le sue combinazioni di cattura e il
tasto Meta. Un programma che usa il mouse tramite il driver del mouse del DOS
segue il puntatore del desktop ovunque si trovi sull'immagine. Un programma
che legge il mouse da sé lo prende quando si fa clic sull'immagine e lo
restituisce quando si preme brevemente il tasto Meta.

Chiudere la finestra della libreria lascia in funzione le macchine accese, e
Libreria delle macchine, nel menu della barra delle icone, la riapre. Ogni
macchina gira in un processo di lavoro tutto suo che non detiene alcuna
autorità: dei suoi programmi e dei suoi dischi non ci si fida mai.

## OPTIONS

`-h`, `-?`, `--help`
: Scrivere questo aiuto sullo standard output e uscire.

## EXIT STATUS

Zero una volta chiusa l'ultima finestra. Diverso da zero quando sono stati
rifiutati il canale della finestra, la casella degli eventi o la sessione del
desktop; il motivo è indicato sullo standard error.
