## NAME

OpenMicro — Open Micro Emulator, emulatore di microcomputer a 8 bit

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Esegue microcomputer a 8 bit in finestre del desktop — il BBC Micro Model B e
l'Acorn Electron — ciclo per ciclo alla loro velocità, ciascuno dotato del
proprio sistema operativo e del proprio BASIC scritti in camera bianca e, sul
Model B, di un file system su disco, così che una macchina si avvia al prompt
del BASIC. L'immagine è quella di un televisore, quattro unità di larghezza
ogni tre di altezza, e il suono passa per l'audio del desktop.

La finestra della libreria mostra ogni macchina come un riquadro con il suo
ultimo schermo. Nuova crea una macchina, con il nome richiesto: un
BBC Micro Model B, con un nuovo disco vuoto nell'unità 0 o senza, oppure un
Acorn Electron, con o senza un Plus 1 montato. Avvia, un doppio clic o il
tasto Invio avvia la macchina scelta in una finestra tutta sua, o ne porta in
primo piano la finestra; si conservano fino a venti macchine, e diverse
funzionano insieme. Duplica copia una macchina con il suo stato salvato e la
sua cartella, ed Elimina elimina una macchina e il suo stato salvato una volta
confermato, lasciandone la cartella. Un clic destro sul riquadro di una
macchina offre Avvia, Impostazioni, Duplica ed Elimina per quella macchina e,
mentre è in funzione, anche il menu della macchina. Al primo avvio l'emulatore
crea un BBC Micro Model B con un disco vuoto.

Impostazioni cambia il nome di una macchina, il suo volume e il caricamento
rapido da nastro, che fa girare la macchina il più veloce possibile mentre il
motore del registratore gira. Ciò che vi è montato si può cambiare solo mentre
è spenta: la ROM in ogni zoccolo — l'OS del Model B e i suoi sedici slot
laterali, l'OS e il BASIC dell'Electron, e la ROM e le cartucce del suo
Plus 1 —, ciascuno con una ROM fornita, RAM laterale in uno slot del Model B,
niente in uno slot o in una cartuccia, o una tua immagine di ROM, messa
nell'archivio dell'emulatore con Installa un'immagine; il Plus 1 dell'Electron;
e i ponticelli della tastiera del Model B, che fissano la modalità dello
schermo all'avvio, il modo in cui si avvia un disco e i tempi del disco.

Chiudere la finestra di una macchina la sospende: riavviata, riprende da dove
si era fermata. Spegni nel suo menu ne toglie i dischi e il nastro e la spegne,
conservandone solo l'ultima schermata.

Ogni macchina ha una cartella tutta sua, Open Micro e poi il nome della
macchina, nella tua cartella Documents. Nuovo disco e Nuovo nastro vi creano un
disco o un nastro vuoto e lo inseriscono, e Dalla cartella offre i dischi e i
nastri che vi si trovano già. I dischi e i nastri della sua cartella che le sue
unità e il suo registratore contengono restano con la macchina: se è stata
spenta, li reinserisce quando si avvia; se viene ripresa, riscrive solo su
quelli che nessun altro programma ha modificato da quando è stata sospesa.

Un'immagine di disco (`.ssd`, `.dsd`) o di nastro (`.uef`, `.csw`) aperta con
l'emulatore va alla macchina accesa la cui finestra è stata attiva per ultima,
o a un'altra macchina accesa che la accetta, o altrimenti alla prima macchina
della libreria che la accetta, avviata apposta. Una scelta con Inserisci disco
o Inserisci nastro entra in quell'unità o nel registratore, e una trascinata
sulla finestra di una macchina entra nell'unità 0 o nel registratore, protetta
da scrittura. Ciò che la macchina scrive su un disco torna nel file del disco
finché quel file può essere scritto; un disco il cui file non può esserlo è
protetto da scrittura. Espelli toglie un disco, e Salva disco come ne tiene una
copia dove si vuole. Un nastro su cui si è registrato viene salvato quando lo
si toglie. Salva istantanea conserva l'intera macchina in un file, e
Ripristina istantanea la riprende, con i suoi dischi e il suo nastro.

Il menu, aperto con un clic destro sull'immagine, offre ogni unità su un
Model B e il nastro; BREAK, Reimposta, Pausa e la velocità; lo schermo intero,
il ridimensionamento nitido o morbido e le linee di scansione; la mappatura
della tastiera; Incolla, che digita il testo degli appunti; le istantanee;
Impostazioni; Libreria delle macchine; e Spegni. Per carattere, i tasti
scrivono ciò che mostrano, e il CAPS LOCK della macchina decide se una lettera
è maiuscola. Per posizione, ogni tasto è il tasto della macchina al suo posto.
Su un Model B, da F1 a F9 sono da f1 a f9, F10 è f0, Inizio è SHIFT LOCK e Ins
è il tasto della barra rovesciata. Su un Electron, da F1 a F10 sono FUNC con i
tasti da 1 a 9 e 0, Alt è FUNC e Bloc Maiusc è CAPS LK. Su entrambi, F12 è
BREAK, Fine è COPY e le frecce sono i tasti cursore.

Delle spie in fondo alla finestra di una macchina mostrano CAPS LOCK e il
motore del registratore e, su un Model B, SHIFT LOCK e ogni unità. Ogni
macchina gira in un processo ausiliario tutto suo privo di qualsiasi autorità,
quindi né il suo software né i suoi supporti sono considerati affidabili; un
processo che fallisce viene sostituito, la macchina riparte come all'accensione
e il motivo viene mostrato.

Chiudere la finestra della libreria lascia in funzione le macchine accese, e
chiudere tutte le finestre lascia l'emulatore nella barra delle icone: un clic
sulla sua icona, o Libreria delle macchine nel suo menu, riapre la libreria, ed
Esci in quel menu sospende tutte le macchine accese e termina l'emulatore.

## OPTIONS

`-h`, `-?`, `--help`
: Scrive questo aiuto sullo standard output ed esce.

## EXIT STATUS

Zero quando si sceglie Esci nel menu della sua icona nella barra delle icone.
Diverso da zero quando il canale delle finestre, la casella degli eventi o la
sessione del desktop sono stati rifiutati; il motivo è indicato sullo standard
error.
