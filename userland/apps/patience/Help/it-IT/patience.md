## NAME

patience — Klondike, FreeCell e Spider, con un risolutore dietro ogni suggerimento

## SYNOPSIS

`patience`

## DESCRIPTION

Apre una finestra della scrivania disposta per una partita di pazienza, i giochi
di carte chiamati anche solitari. Sei giochi sono nel menu Game, e i tasti da
`1` a `6` li scelgono: il Klondike pescando una carta o tre, il FreeCell e lo
Spider con uno, due o quattro semi.

Nel Klondike, costruite le sette colonne in ordine decrescente alternando i
colori, e mandate ogni seme alla sua fondazione, dall'asso al re. Fate clic sul
mazzo per girare carte sugli scarti; quando è vuoto, fate clic sul suo posto per
rigirare gli scarti. Nel FreeCell tutte le carte sono distribuite scoperte e
ognuna delle quattro celle libere tiene una carta; una scala si sposta tutta
insieme attraverso le celle libere e le colonne vuote finché c'è posto. Nello
Spider, costruite in ordine decrescente in qualsiasi seme, ma spostate una scala
solo se è tutta di un seme; una scala completa dal re all'asso va da sola alle
fondazioni. Fate clic sul mazzo per distribuire una carta su ogni colonna, il
che richiede prima che ogni colonna ne abbia una.

Trascinate le carte dove volete, oppure fateci clic: un clic manda le carte nel
posto migliore in cui possono andare, prima alle fondazioni, poi su un'altra
colonna, poi in una colonna vuota, poi in una cella libera. Le carte che non
possono andare da nessuna parte fanno un saltello dove sono. Le carte di cui
nessuna mossa successiva potrebbe avere bisogno vanno da sole alle fondazioni, e
appena nulla può più impedire la vittoria ci vanno tutte. Con Send cards home
disattivato, Finish (`F`) ve le manda.

Ogni suggerimento viene da un risolutore che gioca la partita in anticipo su di
voi, quindi un suggerimento (`H`) è una mossa che mantiene la partita vincibile,
non soltanto una che le regole consentono. Il risolutore esamina ogni posizione
che raggiungete. Quando dimostra che non resta alcuna linea vincente, la barra
di stato lo dice, e Back to last winnable (`Ctrl+B`) vi riporta all'ultima
posizione da cui la partita si poteva ancora vincere.

Con Winnable deals only attivo, come all'inizio, ogni nuova smazzata è una che
il risolutore ha già vinto, e un segno di spunta accanto al suo numero lo
indica. Il risolutore rinuncia a una smazzata che non riesce a decidere in
fretta, e quella che vi dà allora è segnata come non verificata; lo Spider a
quattro semi è spesso troppo profondo per essere deciso in tempo. Ogni smazzata
è numerata nella barra di stato, e il menu proprio del tavolo, aperto con il
tasto secondario, gioca una smazzata dal suo numero. Le smazzate del FreeCell
sono numerate come negli altri giochi di FreeCell, così una smazzata numerata è
la stessa partita ovunque.

Il Klondike si conta nel modo standard, oppure alla maniera di Las Vegas, dove
ogni partita costa 52 $ e ogni carta mandata alle fondazioni vince 5 $, con un
solo passaggio nel mazzo pescando una carta e tre pescandone tre, oppure alla
maniera di Las Vegas cumulativa, che porta le vincite da una partita all'altra.
Lo Spider parte da 500 punti, ne perde uno per ogni mossa e ne guadagna 100 per
ogni scala mandata alle fondazioni. Per ogni gioco le statistiche conservano le
partite giocate e vinte, la serie in corso di vittorie o sconfitte, la serie di
vittorie più lunga, la vittoria più rapida e la vittoria con meno mosse.
Ridistribuire prima di aver vinto una partita la conta come persa.

La partita in corso viene conservata: chiudere la finestra o uscire la rimanda
alla volta successiva, e l'orologio si ferma mentre la finestra è nascosta. Il
menu del gioco nella barra delle icone e il menu proprio del tavolo raccolgono
le opzioni: le smazzate vincibili, il punteggio, l'invio delle carte alle
fondazioni, i semi a quattro colori, il dorso delle carte e il tavolo.

La tastiera fa tutto ciò che fa il puntatore. I tasti freccia spostano un anello
da pila a pila e lungo le carte di una colonna; `Space` raccoglie le carte e le
posa dove si trova l'anello, `Enter` le manda nel posto migliore e `Escape` le
rimette a posto. `D` gira il mazzo, `Ctrl+Z` annulla una mossa e `Ctrl+Y` la
rifà, `N` distribuisce una partita nuova, `R` ridistribuisce questa e `S` mostra
le statistiche.

Il gioco si avvia dalla libreria dei programmi della scrivania, sotto Games,
oppure per nome da una shell. Richiede una sessione grafica attiva: senza di
essa il canale della finestra è irraggiungibile e il gioco segnala il rifiuto
sul flusso di errore standard ed esce.

## EXIT STATUS

Zero dopo un'uscita pulita; diverso da zero quando il canale della finestra o la
regione di frame condivisa è stata rifiutata (il motivo è indicato sul flusso di
errore standard).
