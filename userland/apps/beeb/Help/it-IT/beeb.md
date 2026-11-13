## NAME

beeb — emulatore del BBC Micro modello B

## SYNOPSIS

`beeb`

## DESCRIPTION

Esegue un BBC Micro modello B in una finestra del desktop, ciclo per ciclo
alla sua velocità, con il suo MOS, il suo BBC BASIC e il suo DFS scritti in
camera bianca, così che si avvia al prompt del BASIC con un file system su
disco. L'immagine è quella di un televisore, quattro unità di larghezza ogni
tre di altezza, e il suono passa per l'audio del desktop.

Un'immagine di disco (`.ssd`, `.dsd`) o di nastro (`.uef`, `.csw`) aperta con
l'emulatore, trascinata sulla sua finestra o scelta con Inserisci disco o
Inserisci nastro nel suo menu entra nell'unità o nel registratore. Ciò che la
macchina scrive su un disco torna nel file del disco finché quel file può
essere scritto; un disco il cui file non può esserlo è protetto da scrittura.
Espelli toglie un disco, e Salva disco come ne tiene una copia dove si vuole.
Un nastro su cui si è registrato viene salvato quando lo si toglie. Salva
istantanea conserva l'intera macchina in un file, e Ripristina istantanea la
riprende, con i suoi dischi e il suo nastro.

Il menu, aperto con un clic destro sull'immagine o dall'icona dell'emulatore
nella barra delle icone, offre anche BREAK, Reimposta, Pausa e la velocità;
lo schermo intero, il ridimensionamento nitido o morbido e le linee di
scansione; la mappatura della tastiera; e Incolla, che digita il testo degli
appunti. Per carattere, i tasti scrivono ciò che mostrano, e il CAPS LOCK
della macchina decide se una lettera è maiuscola. Per posizione, ogni tasto è
il tasto del BBC al suo posto: da F1 a F9 sono da f1 a f9, F10 è f0, F12 è
BREAK, Fine è COPY e Inizio è SHIFT LOCK.

Impostazioni sceglie cosa contengono lo zoccolo del sistema e i sedici slot
laterali — le ROM fornite, RAM laterale o immagini di ROM proprie, installate
nell'archivio dell'emulatore —, la modalità dello schermo, l'avvio e i tempi
del disco dei ponticelli della tastiera, l'immagine, i tasti, il volume e il
caricamento rapido da nastro, che fa girare la macchina il più veloce
possibile mentre il motore del registratore gira. Le modifiche alle ROM e ai
ponticelli hanno effetto alla reimpostazione successiva.

Delle spie in fondo alla finestra mostrano CAPS LOCK, SHIFT LOCK, il motore
del registratore e ogni unità. La macchina gira in un processo ausiliario
privo di qualsiasi autorità, quindi né il suo software né i suoi supporti
sono considerati affidabili; un processo che fallisce viene sostituito, la
macchina riparte come all'accensione e il motivo viene mostrato.

## OPTIONS

`-h`, `-?`, `--help`
: Scrive questo aiuto sullo standard output ed esce.

## EXIT STATUS

Zero quando la sua finestra viene chiusa o si sceglie Esci nel menu della sua icona nella barra delle icone. Diverso da zero quando il canale delle finestre, la casella degli eventi o la sessione del desktop sono stati rifiutati; il motivo è indicato sullo standard error.
