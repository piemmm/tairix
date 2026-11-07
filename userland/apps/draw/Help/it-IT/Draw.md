## NAME

Draw — editor di disegni vettoriali

## SYNOPSIS

`Draw`

## DESCRIPTION

Disegna e modifica disegni vettoriali in una finestra del desktop: tracciati,
rettangoli, ellissi, stelle e poligoni, linee, curve a mano libera e testo,
in gruppi e livelli, su una pagina. Avviato con un documento — dal gestore di
file, dal desktop, o trascinando un file sulla sua icona nella barra delle
icone — apre una finestra su di esso. Avviato da solo apre un disegno nuovo
su una pagina A4. Ogni documento è una finestra dell'unico programma;
chiudere l'ultima lo lascia nella barra delle icone, e la riga Esci del suo
menu d'icona lo termina.

Draw apre e salva SVG. Un disegno conserva la sua pagina e le sue unità, i
suoi livelli, gruppi e ritagli, ogni tracciato, forma e testo, i colori pieni
e le sfumature lineari e radiali, l'opacità di ogni oggetto, i tratteggi, le
giunzioni, le estremità e le punte di freccia, le guide e la griglia, e il
titolo del disegno. Un file che contiene ciò che un disegno non può
conservare — effetti di filtro, maschere, riempimenti a motivo, marcatori,
immagini incorporate, stili di testo misti, animazione o script, tra gli
altri — si apre in sola lettura, con la banda di stato che nomina ciò che
contiene, e Salva chiede un nome nuovo perché l'originale non venga mai
sovrascritto. Salva una copia scrive il disegno come PDF, come PNG — la
pagina alla sua dimensione, fino a 8192 pixel sul lato più lungo — o come
SVG, lasciando com'è il file del documento.

La cassetta degli strumenti scende lungo il lato sinistro della finestra,
l'ispettore lungo il destro, e la banda di stato corre in fondo: mostra la
posizione del puntatore e la dimensione della selezione nelle unità della
pagina, e dice che cosa ha fatto l'ultimo comando. In alto, la barra dice
come si usa lo strumento in mano e mostra lo zoom: premere lo zoom apre un
menu di ingrandimenti. Righelli nelle unità della pagina corrono lungo il
lato superiore e sinistro della tela, e barre di scorrimento lungo il destro
e il basso. Soffermarsi sul pulsante di uno strumento lo nomina, con il
tasto che lo sceglie.

Gli strumenti, e i tasti che li scelgono, sono: selezione (`V`), nodi (`A`),
rettangolo (`R`), ellisse (`E`), stella e poligono (`S`), linea (`L`),
penna (`B`), matita (`P`), testo (`T`), contagocce (`I`), zoom (`Z`) e
mano (`H`).

Lo strumento di selezione prende un oggetto con una pressione, un gruppo
intero, e Maiusc aggiunge un oggetto alla selezione o lo toglie. Una
pressione sulla tela vuota trascina un riquadro che seleziona ciò che vi sta
interamente dentro, o con Alt tutto ciò che tocca. Trascinare la selezione la
sposta, lungo un solo asse con Maiusc, e con Alt sposta una copia. Le sue
otto maniglie la scalano dalla maniglia opposta, un angolo con Maiusc
mantenendo le proporzioni; trascinare appena fuori da un angolo la ruota
attorno al suo centro, con Maiusc a passi di 15 gradi. I tasti freccia la
spostano di un pixel, e di dieci con Maiusc.

Lo strumento nodi mostra i nodi del tracciato selezionato e le maniglie delle
sue curve: trascini un nodo per spostarlo, o una maniglia per piegarne la
curva; l'altra maniglia di un nodo morbido segue allineata. Rettangoli,
ellissi, stelle e linee si disegnano trascinando: Maiusc rende uguali i lati
— un quadrato, un cerchio, una linea a un multiplo di 15 gradi — e Alt
disegna a partire dal punto in cui è iniziato il trascinamento. Una stella si
disegna dal suo centro; le punte, il raggio interno e se è un poligono si
impostano nella scheda Forma dell'ispettore, come gli angoli di un rettangolo
nuovo.

La penna traccia un percorso nodo per nodo: faccia clic per un angolo, o
trascini per un nodo morbido e la sua curva. Un clic sul primo nodo chiude il
tracciato, e Invio, o la scelta di un altro strumento, lo termina aperto;
`Delete` o `Backspace` riprende l'ultimo nodo, ed Esc scarta il tracciato.
La matita disegna a mano libera, e la sua linea viene adattata a curve
morbide quando la si rilascia. Lo strumento testo comincia un testo dove si
fa clic, o modifica il testo cliccato: scriva, Invio comincia una riga, i
tasti freccia, Home e Fine spostano il cursore, ed Esc, o un clic altrove,
termina la scrittura come un'unica modifica; un testo lasciato vuoto viene
rimosso. Il contagocce dà alla selezione, e a ciò che si disegnerà dopo, lo
stile dell'oggetto cliccato. Lo strumento zoom ingrandisce dove si fa clic,
rimpicciolisce con Alt, e un riquadro trascinato con esso riempie la
finestra. La mano trascina la tela, come fa un trascinamento con il pulsante
centrale o con Spazio premuto, con qualunque strumento.

La rotella scorre la tela, in orizzontale con Maiusc; con Ctrl ingrandisce
attorno al puntatore, come un pizzico su un touchpad o un touchscreen.
Visualizza ▸ Adatta pagina e Adatta selezione le fanno stare nella finestra,
e Dimensione reale mostra il disegno al 100 per cento; lo zoom va dall'1 per
cento al 25600.

L'ispettore ha quattro schede. Stile imposta il riempimento e il contorno
della selezione — e, se nulla è selezionato, di ciò che si disegnerà dopo:
scelga Riempimento o Contorno, poi Nessuno o un colore dal selettore di
colore, che offre un quadrato di saturazione e valore accanto a una striscia
di tinte, una ruota o dei cursori, e i valori e l'opacità del colore; poi lo
spessore del contorno, le sue giunzioni, ad angolo, arrotondate o smussate,
le sue estremità, piatte, arrotondate o quadrate, i suoi tratteggi, continuo,
tratteggiato, punteggiato o tratto-punto, le punte di freccia all'inizio e
alla fine — ogni pressione passa per nessuna, triangolo, freccia, cerchio e
rombo —, la regola di riempimento, diversa da zero o pari-dispari, e
l'opacità di ogni oggetto selezionato. Livelli elenca i livelli, il più alto
per primo: ne prema uno per disegnarvi, e il suo pulsante Visibile per
mostrarlo o nasconderlo; sopra l'elenco, Nuovo aggiunge un livello, Rimuovi
rimuove il livello in mano, Blocca lo protegge dalle modifiche, e Su e Giù lo
spostano. Oggetto colloca la selezione per il suo lato sinistro e superiore e
la scala a una larghezza e un'altezza, la allinea — per i bordi sinistri, i
centri o i bordi destri, o per l'alto, il centro o il basso — e distribuisce
tre o più oggetti in modo uniforme in orizzontale o in verticale. Gli oggetti
si allineano nella loro selezione; un solo oggetto, o qualunque numero con
Allinea alla pagina attivo, nella pagina. Forma contiene gli angoli di un
rettangolo, le punte e il raggio interno di una stella e se è un poligono, e
la famiglia del testo — senza grazie, con grazie o a spaziatura fissa —, la
dimensione, il grassetto, il corsivo e l'allineamento sulla sua origine. Una
scheda più alta della finestra scorre. Un colore che si sta scegliendo, uno
spessore digitato o regolato a passi e un'opacità si vedono sulla tela mentre
cambiano e diventano un'unica modifica da annullare quando si assestano; Tab
passa al campo successivo e Maiusc+Tab al precedente.

Un trascinamento da un righello pone una guida orizzontale o verticale sulla
tela; trascini una guida per spostarla, o la riporti su un righello per
toglierla. Visualizza ▸ Griglia mostra la griglia della pagina. Finché Aggancio
è attivo, il puntatore si aggancia quando si disegna una forma, si pone un
nodo con la penna o si sposta o si scala la selezione: alle guide, ai bordi e
al centro della pagina, e alla griglia quando è mostrata. Visualizza ▸
Contorni mostra ogni oggetto come il suo contorno, il che rende leggibile un
disegno affollato. Il menu Pagina imposta la carta della pagina — A4, A3, A5,
A6, A2, A1, A0, B5, B4, US Letter, US Legal, Tabloid, Executive, due
dimensioni di schermo e un'icona —, la gira tra verticale e orizzontale, le
dà uno sfondo bianco o nessuno, e ne imposta le unità: pixel, punti, pica,
millimetri, centimetri o pollici.

Oggetto ▸ Raggruppa riunisce la selezione in un gruppo e Separa lo scioglie;
Porta in primo piano, Su, Giù e Porta in fondo riordinano gli oggetti nel
loro livello; Rifletti e Ruota specchiano la selezione o la ruotano di un
quarto di giro. Blocca e Nascondi mettono gli oggetti fuori portata, e
Sblocca tutto e Mostra tutto nel livello riportano quelli del livello in
mano. Il menu Tracciato unisce le forme selezionate in una (Unione), ritaglia
quelle sopra dalla più bassa (Differenza), conserva dove si sovrappongono
(Intersezione) o dove no (Esclusione), trasforma forme e testo in tracciati i
cui nodi si possono modificare, e trasforma i contorni in forme riempite.
Queste operazioni si calcolano a parte rispetto alla finestra, che resta
libera nel frattempo; un disegno modificato prima che arrivi il risultato
conserva la sua modifica, e il risultato non viene applicato.

Ogni modifica è un passo da annullare, e Annulla e Ripeti nominano il passo
che compiono; si conservano fino a 1000 passi, meno quando manca memoria.
Copia mette la selezione negli appunti come SVG, e Incolla aggiunge ciò che
contengono gli appunti al livello in mano, al suo posto sulla pagina,
selezionato.

Draw non detiene alcuna capacità sul file system. Modifica solo il file che
gli è stato consegnato: un file che l'utente può cambiare gli è consegnato
scrivibile e Salva lo riscrive, e ogni altro si apre in sola lettura e Salva
chiede dove salvarlo. I documenti, e i disegni incollati dagli appunti,
vengono letti in un processo di lavoro separato senza alcuna portata, così
che un file ostile non può toccare nulla di ciò che Draw raggiunge.

Premere il pulsante secondario (destro) del mouse in qualunque punto della
finestra apre il suo menu: File, Pagina, Modifica, Oggetto, Tracciato,
Livello, Visualizza e Strumenti, ognuno un sottomenu. La finestra non ha una
barra dei menu. Chiudere una finestra, o uscire, con modifiche non salvate
chiede prima. Chiudere una finestra con il pulsante secondario sul suo
pulsante di chiusura mostra anche il suo file nel gestore di file, nella
cartella che lo contiene.

* `Ctrl+N` — una finestra nuova; `Ctrl+O` — aprire un file
* `Ctrl+S` — salvare; `Ctrl+Shift+S` — salvare con nome; `Ctrl+Shift+E` — salvare una copia come PDF, PNG o SVG
* `Ctrl+W` — chiudere la finestra
* `Ctrl+Z` — annullare; `Ctrl+Shift+Z` o `Ctrl+Y` — ripetere
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — tagliare, copiare, incollare; `Ctrl+D` — duplicare
* `Delete` o `Backspace` — eliminare la selezione
* `Ctrl+A` — selezionare tutto; `Ctrl+Shift+A` — non selezionare nulla; `Escape` — non selezionare nulla, o scartare il tracciato in corso
* `Ctrl+G` — raggruppare; `Ctrl+Shift+G` — separare
* `Page Up` / `Page Down` — portare su o giù; `Home` / `End` — portare in primo piano o in fondo
* `Ctrl+[` / `Ctrl+]` — ruotare a sinistra o a destra
* `Ctrl+L` — bloccare; `Ctrl+Shift+L` — sbloccare tutto nel livello
* `Ctrl+H` — nascondere; `Ctrl+Shift+H` — mostrare tutto nel livello
* `Ctrl+Shift+C` — oggetto in tracciato
* `Ctrl+Shift+N` — un livello nuovo; `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — portare il livello su o giù
* `Ctrl+=` o `Ctrl++` / `Ctrl+-` — ingrandire o rimpicciolire; `Ctrl+1` — dimensione reale; `Ctrl+0` — adattare la pagina
* `Ctrl+'` — mostrare o nascondere la griglia; `%` — aggancio sì o no
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `I`, `Z`, `H` — gli strumenti, in ordine
* tasti freccia — spostare la selezione di un pixel; con `Shift`, di dieci
* `Space` — premuto, trascinare la tela con qualunque strumento
* `Ctrl` + rotella — ingrandire attorno al puntatore; `Shift` + rotella — scorrere in orizzontale

## OPTIONS

`-h`, `-?`, `--help`
: Scrive questa guida sull'output standard ed esce.

## EXIT STATUS

Zero dopo Esci. Diverso da zero quando il canale della finestra, la casella
degli eventi o la sessione del desktop è stata rifiutata; il motivo è indicato
sull'output di errore standard.
