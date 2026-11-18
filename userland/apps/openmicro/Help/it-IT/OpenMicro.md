## NAME

OpenMicro — Open Micro Emulator, emulatore di microcomputer a 8 bit

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Esegue microcomputer a 8 bit in finestre del desktop — il BBC Micro Model B,
l'Acorn Electron e il Commodore 64 — ciclo per ciclo alla loro velocità,
ciascuno dotato del proprio sistema operativo e del proprio BASIC scritti in
camera bianca e, sul Model B, di un file system su disco e, sul Commodore 64,
di un'unità a dischetti 1541 con il proprio DOS, così che una macchina si avvia
al prompt del BASIC. L'immagine è quella di un televisore, quattro unità di
larghezza ogni tre di altezza, e il suono passa per l'audio del desktop.

La finestra della libreria mostra ogni macchina come un riquadro con il suo
ultimo schermo o, finché non è stata in funzione, con la schermata di avvio
del suo modello. Nuova crea una macchina, con il nome richiesto: un
BBC Micro Model B, con RAM laterale negli slot da 4 a 7 e un nuovo disco vuoto
nell'unità 0 o senza; un Acorn Electron, con o senza un Plus 1 montato; oppure
un Commodore 64, PAL o NTSC, con un SID 6581 o 8580 e un'unità 1541, due o
nessuna. Avvia, un doppio clic o il tasto Invio avvia la macchina scelta in
una finestra tutta sua, o ne porta in primo piano la finestra; si conservano
fino a venti macchine, e diverse funzionano insieme. Duplica copia una
macchina con il suo stato salvato e la sua cartella, ed Elimina elimina una
macchina e il suo stato salvato una volta confermato, lasciandone la cartella.
Un clic destro sul riquadro di una macchina offre Avvia, Impostazioni,
Duplica, Elimina e Mostra cartella per quella macchina e, mentre è in
funzione, anche il menu della macchina. Al primo avvio l'emulatore crea un
BBC Micro Model B con un disco vuoto.

Impostazioni cambia il nome di una macchina, il suo volume, il caricamento
rapido da nastro, che fa girare la macchina il più veloce possibile mentre il
motore del registratore gira, e, su un Commodore 64, la porta di controllo in
cui è inserito il suo joystick sul tastierino. Ciò che vi è montato si può
cambiare solo mentre è spenta: la ROM in ogni zoccolo — l'OS del Model B e i
suoi sedici slot laterali, l'OS e il BASIC dell'Electron, la ROM e le cartucce
del suo Plus 1 e, sul Commodore 64, il KERNAL, il BASIC, il set di caratteri e
il DOS di ogni unità —, ciascuno con una ROM fornita, RAM laterale in uno slot
del Model B, niente in uno slot o in una cartuccia, o una tua immagine di ROM,
messa nell'archivio dell'emulatore con Installa un'immagine; il Plus 1
dell'Electron; lo standard video, il SID e le unità del Commodore 64; e i
ponticelli della tastiera del Model B, che fissano la modalità dello schermo
all'avvio, il modo in cui si avvia un disco e i tempi del disco.

Un Model B ha anche VIM, un editor nello stile di vi, montato nello slot 13. Al
prompt della macchina, `*VIM` seguito dal nome di un file modifica quel file, e
BREAK in VIM conserva il testo che contiene. VIM modifica testi, come i file
`!BOOT` che crea `*BUILD`, e programmi BASIC, che elenca come testo e salva di
nuovo tokenizzati, colorando ciascuno per quanto lo consentono i colori della
modalità dello schermo; `:dis` seguito dal nome di un file scorre pagina per
pagina il codice macchina di un programma. In VIM, `:help` elenca i suoi
comandi e le modalità dello schermo con lo spazio che ciascuna lascia, `:mode`
seguito da un numero cambia la modalità, e `:q` torna al BASIC. Dove un banco di
RAM laterale non contiene alcuna ROM, VIM vi tiene ciò che copia o cancella, nel
banco più alto fra questi, così da non togliere nulla alla memoria del testo.

Chiudere la finestra di una macchina la sospende: riavviata, riprende da dove
si era fermata. Spegni nel suo menu ne toglie i supporti e la spegne,
conservandone solo l'ultima schermata.

Ogni macchina ha una cartella tutta sua, Open Micro e poi il nome della
macchina, nella tua cartella Documents. Nuovo disco, Nuovo dischetto su un
Commodore 64, e Nuovo nastro vi creano un disco, un dischetto o un nastro vuoto
e lo inseriscono, Dalla cartella offre i dischi, i dischetti e i nastri che vi
si trovano già, e Salva istantanea vi conserva l'intera macchina come file
`.snapshot`. Mostra cartella apre la cartella in Files, così ciò che la
macchina vi conserva può essere copiato su altri supporti o su un altro
computer. I dischi, i dischetti e i nastri della sua cartella che le
sue unità e il suo registratore contengono restano con la macchina: se è stata
spenta, li reinserisce quando si avvia; se viene ripresa, riscrive solo su
quelli che nessun altro programma ha modificato da quando è stata sospesa.

Un file aperto con l'emulatore — un'immagine di disco (`.ssd`, `.dsd`) o di
nastro (`.uef`, `.csw`) o, per un Commodore 64, un dischetto (`.d64`, `.g64`),
un nastro (`.tap`), una cartuccia (`.crt`) o un programma (`.prg`, `.t64`) — va
alla macchina accesa la cui finestra è stata attiva per ultima, o a un'altra
macchina accesa che lo accetta, o altrimenti alla prima macchina della libreria
che lo accetta, avviata apposta, e la finestra di quella macchina passa in
primo piano. Aprire un disco del BBC Micro in Files ne mostra i file invece di
avviare una macchina. Uno scelto con Inserisci disco, Inserisci dischetto o
Inserisci nastro entra in quell'unità o nel registratore, e uno trascinato
sulla finestra di una macchina entra nella sua prima unità o nel registratore,
protetto da scrittura, e la finestra prende la tastiera. Ciò che la macchina
scrive su un disco o un dischetto torna nel suo file finché quel file può
essere scritto, e mentre il supporto è nell'unità né un montaggio né un'altra
macchina possono scrivere quel file; un disco o un dischetto il cui file non
può esserlo, o che un altro programma sta usando, è protetto da scrittura, e
la finestra ne dice il motivo. Per un dischetto `.d64` che è arrivato a
richiedere un `.g64` viene proposto di salvarlo in quel formato. Espelli
toglie un disco o un dischetto, e Salva disco come o Salva dischetto come ne
tiene una copia dove si vuole. Un nastro su cui si è registrato viene salvato
quando lo si toglie. Su un Commodore 64, Riproduci, Registra, Ferma e
Riavvolgi nel menu Nastro comandano il suo datasette; Inserisci cartuccia
innesta una cartuccia e Rimuovi cartuccia la toglie, riavviando ogni volta la
macchina; e Carica programma mette un programma in memoria, come farebbe LOAD,
perché RUN lo avvii. Salva istantanea conserva l'intera macchina in un file —
nella sua cartella o, prima che ne abbia una, dove scegli —, e Ripristina
istantanea riprende una macchina così conservata, con i suoi supporti.

Il menu, aperto con un clic destro sull'immagine, offre ogni unità su un
Model B o su un Commodore 64 e il nastro, e su un Commodore 64 la cartuccia e
Carica programma; BREAK, o RESTORE su un Commodore 64; su un Model B e su un
Electron, SHIFT-BREAK, che preme BREAK tenendo premuto SHIFT per avviare il
disco o il nastro; Reimposta, Pausa e la velocità; lo schermo intero, il
ridimensionamento nitido o morbido e le linee di scansione; la mappatura della
tastiera; Incolla, che digita il testo degli appunti, ogni lettera maiuscola o
minuscola come la mostra la macchina; le istantanee; Mostra cartella;
Impostazioni; Libreria delle macchine; e Spegni. Per carattere, i tasti
scrivono ciò che mostrano, e il CAPS LOCK della macchina, o su un Commodore 64
il tasto Maiusc, decide se una lettera è maiuscola. Per posizione, ogni tasto
è il tasto della macchina al suo posto. Su un Model B, da F1 a F9 sono da f1 a
f9, F10 è f0, Inizio è SHIFT LOCK e Ins è il tasto della barra rovesciata. Su
un Electron, da F1 a F10 sono FUNC con i tasti da 1 a 9 e 0, Alt è FUNC e Bloc
Maiusc è CAPS LK. Su entrambi, F12 è BREAK, Fine è COPY e le frecce sono i
tasti cursore. Su un Commodore 64, F1, F3, F5 e F7 sono i suoi tasti funzione
e F2, F4, F6 e F8 gli stessi con SHIFT, Esc è RUN/STOP, Alt è il tasto
Commodore, Ctrl è CTRL, Inizio è CLR/HOME, F12 o PagSu è RESTORE e le frecce
sono i tasti cursore; con il suo joystick sul tastierino attivo, le frecce del
tastierino numerico muovono il joystick e i tasti 0 e 5 sparano.

Delle spie in fondo alla finestra di una macchina mostrano CAPS LOCK e il
motore del registratore e, su un Model B, SHIFT LOCK e ogni unità; su un
Commodore 64 mostrano il motore del datasette e ogni unità. Su un Model B e su
un Electron, il pulsante SHIFT-BREAK in fondo a destra fa ciò che fa la riga
del menu. Ogni macchina gira in un processo ausiliario tutto suo privo di
qualsiasi autorità, quindi né il suo software né i suoi supporti sono
considerati affidabili; un processo che fallisce viene sostituito, la macchina
riparte come all'accensione e il motivo viene mostrato.

Chiudere la finestra della libreria lascia in funzione le macchine accese, e
chiudere tutte le finestre lascia l'emulatore nella barra delle icone: un clic
sulla sua icona, o Libreria delle macchine nel suo menu, porta sempre in primo
piano la libreria, ed Esci in quel menu sospende tutte le macchine accese e
termina l'emulatore.

## OPTIONS

`-h`, `-?`, `--help`
: Scrive questo aiuto sullo standard output ed esce.

## EXIT STATUS

Zero quando si sceglie Esci nel menu della sua icona nella barra delle icone.
Diverso da zero quando il canale delle finestre, la casella degli eventi o la
sessione del desktop sono stati rifiutati; il motivo è indicato sullo standard
error.
