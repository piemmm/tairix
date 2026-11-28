## NAME

TAIRiXBridge — TAIRiX Bridge, programmi RISC OS sul desktop di TAIRiX

## SYNOPSIS

`TAIRiXBridge`

## DESCRIPTION

Esegue programmi RISC OS — programmi BASIC, strumenti a riga di comando e
giochi a singolo compito — sul desktop di TAIRiX, ciascuno con il proprio
schermo in una finestra tutta sua. Il RISC OS su cui girano è quello di TAIRiX
Bridge, scritto a partire dalla documentazione pubblicata; non contiene codice
di RISC OS.

I programmi girano in sessioni. La sessione condivisa è la macchina RISC OS
mostrata dalla finestra TAIRiX Bridge: il suo disco è la cartella RISC OS nei
tuoi Documenti, la sua sequenza di avvio viene eseguita all'avvio, e un
programma proveniente dalla tua memoria di massa gira in essa. Un programma
proveniente da altrove — supporti rimovibili, un'immagine di disco, una
condivisione di rete — o di origine sconosciuta gira in una sessione isolata
tutta sua, che non vede mai la tua cartella RISC OS e termina con il suo
ultimo programma. Esegui nella sessione condivisa esegue un file nella
sessione condivisa, ovunque si trovi.

La finestra di una sessione elenca i suoi compiti, ciascuno con la propria
memoria e se è in pausa o occupato, e ciò che occupano le aree condivise della sessione.
Esegui sceglie un file da eseguire; anche un file trascinato sulla finestra, o
aperto con TAIRiX Bridge, viene eseguito. Scegliere un compito e poi Pausa o
Esci lo sospende o lo termina.

Lo schermo di ogni programma è una finestra tutta sua, con l'immagine scalata
alla finestra nelle proporzioni del suo modo. Il pulsante Schermo apre il menu
della finestra: Schermo intero, ridimensionamento nitido o morbido, Pausa e
Reimposta, che termina il programma e lo riesegue nella stessa finestra. Lo
schermo prende tutta la tastiera, ogni tasto con il numero che gli dà RISC OS.
Select, Menu e Adjust sono i pulsanti primario, secondario e centrale. Appena
un programma muove da sé il mouse, un clic cattura il mouse per lui, e un
singolo tocco del tasto Meta lo rilascia.

Impostazioni sceglie l'ordine dei pulsanti, la memoria che ogni sessione può
prendere, se ogni programma viene isolato e come vengono scalati gli schermi.
Il processo di lavoro di una sessione non detiene alcuna autorità: i file che
legge e scrive vengono trasferiti per lui, e un processo che fallisce termina
la sua sessione, con il motivo mostrato nella finestra della sessione.

## OPTIONS

`-h`, `-?`, `--help`
: Scrivere questo aiuto sull'output standard e uscire.

## EXIT STATUS

Zero una volta chiusa la finestra dell'ultima sessione. Diverso da zero quando
il canale delle finestre, la casella degli eventi o la sessione del desktop è
stata rifiutata; il motivo è indicato sul flusso di errore standard.
