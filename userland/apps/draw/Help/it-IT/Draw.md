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

Draw apre SVG, i file Draw di RISC OS e i PDF, e salva un disegno come SVG,
come file Draw di RISC OS o come PDF, nel formato che dice il nome del file;
Salva con nome offre tutti e tre. Un file Draw tiene di un disegno ciò che
può, e un PDF si riapre come un solo livello il cui testo è fatto di contorni;
il salvataggio lo dice. Di un PDF si apre una pagina come disegno, con il suo
riquadro di ritaglio come pagina: un PDF di più pagine chiede prima quale,
dando la dimensione di ogni pagina in punti, e uno protetto da una password la
chiede, e dice quando quella data è sbagliata. Il testo che un PDF compone nei
suoi caratteri non viene ancora letto. Un disegno conserva la sua pagina e le
sue unità, i suoi livelli, gruppi e ritagli, ogni tracciato, forma, testo e
immagine, i colori pieni e le sfumature lineari e radiali, l'opacità di ogni
oggetto, i tratteggi, le giunzioni, le estremità e le punte di freccia, le
guide e la griglia, e il titolo del disegno. Un file che contiene ciò che un
disegno non può conservare — effetti di filtro, maschere, riempimenti a
motivo, marcatori, immagini illeggibili, stili o tracciati di testo che un
testo non può tenere, animazione o script, tra gli altri — si apre in sola
lettura, con la banda di stato che nomina ciò che contiene, e Salva chiede un
nome nuovo perché l'originale non venga mai sovrascritto. Salva una copia
scrive il disegno come PDF; come EPS, con la trasparenza resa opaca, come la
dipinge PostScript; come PNG, JPEG o TIFF, alla risoluzione di copia della
pagina e fino a 8192 pixel sul lato più lungo; come SVG; o come file Draw di
RISC OS, nominando ciò che il formato non ha potuto tenere del disegno, come
le sfumature; sempre lasciando com'è il file del documento.

La cassetta degli strumenti scende lungo il lato sinistro della finestra,
l'ispettore lungo il destro, e la banda di stato corre in fondo: dice che cosa
ha fatto l'ultimo comando, altrimenti come si usa lo strumento in mano, e
mostra la posizione del puntatore e la dimensione della selezione nelle unità
della pagina, e lo zoom: premere lo zoom apre un menu di ingrandimenti.
Righelli nelle unità della pagina corrono lungo il lato superiore e sinistro
della tela, e barre di scorrimento lungo il destro e il basso. Soffermarsi sul
pulsante di uno strumento lo nomina, con il tasto che lo sceglie.

Gli strumenti, e i tasti che li scelgono, sono: selezione (`V`), nodi (`A`),
rettangolo (`R`), ellisse (`E`), stella e poligono (`S`), linea (`L`), penna
(`B`), matita (`P`), testo (`T`), sfumatura (`G`), contagocce (`I`), zoom
(`Z`) e mano (`H`).

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
`Delete` o `Backspace` riprende l'ultimo nodo, ed Esc scarta il tracciato. La
matita disegna a mano libera, e la sua linea viene adattata a curve morbide
quando la si rilascia. Lo strumento testo comincia un testo dove si fa clic, o
modifica il testo cliccato: scriva, Invio comincia una riga, i tasti freccia,
Home e Fine spostano il cursore, ed Esc, o un clic altrove, termina la
scrittura come un'unica modifica; un testo lasciato vuoto viene rimosso. Il
contagocce dà alla selezione, e a ciò che si disegnerà dopo, lo stile
dell'oggetto cliccato. Lo strumento sfumatura posa una sfumatura lungo il
trascinamento sulla selezione o, se nulla è selezionato, sull'oggetto premuto:
Maiusc la ruota a passi di 15°, e Alt, o una sfumatura già radiale, la posa
radiale attorno al punto in cui il trascinamento è iniziato; finché lo
strumento è in mano, la sfumatura dell'oggetto selezionato appare come una
linea tra due maniglie. Lo strumento zoom ingrandisce dove si fa clic,
rimpicciolisce con Alt, e un riquadro trascinato con esso riempie la finestra.
La mano trascina la tela, come fa un trascinamento con il pulsante centrale o
con Spazio premuto, con qualunque strumento.

Premuto su un testo, lo strumento testo mette il cursore dove è stato premuto.
Maiusc con le frecce, Inizio e Fine, o un trascinamento sul testo, seleziona
caratteri, e `Ctrl+A` li seleziona tutti; ciò che si digita poi prende il loro
posto, nello stile del primo di essi. Finché ci sono caratteri selezionati, un
carattere tipografico, una dimensione, il grassetto, il corsivo, la spaziatura
tra le lettere, il rialzo o un colore scelti nell'ispettore impostano solo
quelli; senza selezione, impostano tutto il testo, ogni suo carattere. La
scheda Forma tiene la spaziatura tra le lettere di un testo, di quanto sono
rialzati i suoi caratteri, in percentuale della loro dimensione, e
l'interlinea — o, per un testo lungo un tracciato, a che distanza lungo il
tracciato comincia — e mentre si digita l'ispettore mostra come è impostato il
primo carattere selezionato. Una sfumatura dipinge sempre un testo intero.

Tracciato ▸ Metti il testo sul tracciato, con un testo e una forma
selezionati, dispone il testo lungo il contorno della forma, che vi si unisce:
ogni carattere poggia sul tracciato ruotato secondo la sua direzione, un testo
lungo un tracciato è una sola riga e un carattere che cade oltre un'estremità
non viene disegnato. Stacca dal tracciato rimette il testo in righe da dove
cominciava il suo tracciato e restituisce il tracciato come un oggetto a sé.
Oggetto in tracciato trasforma un testo dipinto in più di un modo in un gruppo
di un tracciato per ciascun modo. Lo strumento nodi modifica il tracciato
lungo cui è disposto un testo come qualsiasi tracciato.

La rotella scorre la tela, in orizzontale con Maiusc; con Ctrl ingrandisce
attorno al puntatore, come un pizzico su un touchpad o un touchscreen.
Visualizza ▸ Adatta pagina e Adatta selezione le fanno stare nella finestra,
e Dimensione reale mostra il disegno al 100 per cento; lo zoom va dall'1 per
cento al 25600.

L'ispettore ha cinque schede. Stile imposta il riempimento e il contorno della
selezione — e, se nulla è selezionato, di ciò che si disegnerà dopo: scelga
Riempimento o Contorno, poi Nessuno o un colore dal selettore di colore, che
offre un quadrato di saturazione e valore accanto a una striscia di tinte, una
ruota o dei cursori, e i valori e l'opacità del colore; poi lo spessore del
contorno, le sue giunzioni, ad angolo, arrotondate o smussate, le sue
estremità, piatte, arrotondate o quadrate, i suoi tratteggi, continuo,
tratteggiato, punteggiato o tratto-punto, le punte di freccia all'inizio e
alla fine — ogni pressione passa per nessuna, triangolo, freccia, cerchio e
rombo —, la regola di riempimento, diversa da zero o pari-dispari, e l'opacità
di ogni oggetto selezionato. Livelli elenca i livelli, il più alto per primo:
ne prema uno per disegnarvi, e il suo pulsante Visibile per mostrarlo o
nasconderlo; sopra l'elenco, Nuovo aggiunge un livello, Rimuovi rimuove il
livello in mano, Blocca lo protegge dalle modifiche, e Su e Giù lo spostano.
Oggetto colloca la selezione per il suo lato sinistro e superiore e la scala a
una larghezza e un'altezza, la allinea — per i bordi sinistri, i centri o i
bordi destri, o per l'alto, il centro o il basso — e distribuisce tre o più
oggetti in modo uniforme in orizzontale o in verticale. Gli oggetti si
allineano nella loro selezione; un solo oggetto, o qualunque numero con
Allinea alla pagina attivo, nella pagina. Forma contiene gli angoli di un
rettangolo, le punte e il raggio interno di una stella e se è un poligono, e
la famiglia del testo — senza grazie, con grazie o a spaziatura fissa —, la
dimensione, il grassetto, il corsivo e l'allineamento sulla sua origine. Una
scheda più alta della finestra scorre. Un colore che si sta scegliendo, uno
spessore digitato o regolato a passi e un'opacità si vedono sulla tela mentre
cambiano e diventano un'unica modifica da annullare quando si assestano; Tab
passa al campo successivo e Maiusc+Tab al precedente.

In Stile, Lineare e Radiale dipingono il riempimento o il contorno con una
sfumatura adattata a ciascun oggetto, dal suo colore al bianco, o al nero dove
è chiaro. La barra sotto mostra la sfumatura su una scacchiera, con una
maniglia per ogni punto: prema una maniglia per prendere in mano quel punto —
il suo colore passa allora al selettore di colore — e la trascini lungo la
barra; prema la barra per aggiungervi un punto. Canc, o Rimuovi punto, rimuove
il punto in mano; Sinistra e Destra prendono il punto accanto, e con Maiusc lo
spostano di un centesimo. Punto a colloca il punto in mano, Angolo ruota una
sfumatura lineare, Estendi, Rifletti e Ripeti dicono come una sfumatura
prosegue oltre le sue estremità, e Inverti la fa scorrere al contrario. Sotto
il selettore, i campioni — grigi, poi dodici tinte piene, chiare e scure, poi
quelli del disegno stesso — danno il loro colore al colore o al punto in mano;
Aggiungi campione conserva il colore in mano con il disegno e Rimuovi campione
ne lascia andare uno dei suoi. In Oggetto, Rotazione ruota la selezione in
senso antiorario: un oggetto fino all'angolo dato, più oggetti insieme di
quell'angolo.

Pagina imposta la larghezza e l'altezza della pagina nella sua unità, e se sta
in verticale o in orizzontale; l'unità — px, pt, pc, mm, cm o in; la
spaziatura della griglia in quell'unità, e in quante parti è diviso ciascun
lato dei suoi quadrati; i dpi di copia, la risoluzione a cui si fanno le sue
copie PNG, JPEG e TIFF; e lo sfondo della pagina, Nessuno o un colore da un
selettore di colore tutto suo, mostrato sulla tela mentre lo si sceglie. Una
pagina senza sfondo appare come carta bianca, e una sua copia in PNG è
trasparente ovunque non ci sia nulla di disegnato.

Un trascinamento da un righello pone una guida orizzontale o verticale sulla
tela; trascini una guida per spostarla, o la riporti su un righello per
toglierla. Visualizza ▸ Griglia mostra la griglia della pagina. Finché
Aggancio è attivo, il puntatore si aggancia quando si disegna una forma, si
pone un nodo con la penna o si sposta o si scala la selezione: alle guide, ai
bordi e al centro della pagina, e alla griglia quando è mostrata. Visualizza ▸
Contorni mostra ogni oggetto come il suo contorno, il che rende leggibile un
disegno affollato. Imposta pagina, nel menu Pagina, mostra la scheda Pagina, e
il menu imposta la carta della pagina — A4, A3, A5, A6, A2, A1, A0, B5, B4, US
Letter, US Legal, Tabloid, Executive, due dimensioni di schermo e un'icona —,
la gira tra verticale e orizzontale, le dà lo sfondo della scheda Pagina o
nessuno, e ne imposta le unità: pixel, punti, pica, millimetri, centimetri o
pollici.

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

Oggetto ▸ Ritaglia con l'oggetto in cima mostra il resto della selezione solo
all'interno del suo oggetto più alto, e Rilascia ritaglio mostra di nuovo
intero un gruppo ritagliato, con l'oggetto che lo ritagliava di nuovo sopra
ciò che conteneva. Tracciato ▸ Dividi taglia le forme selezionate nei pezzi
che le loro sovrapposizioni formano. Espandi e Contrai allargano o restringono
ogni forma selezionata dello Scostamento della scheda Forma, Semplifica
ritraccia ciascuna con il minor numero di nodi che la tenga entro la
Morbidezza della scheda, e Inverti direzione fa percorrere un tracciato al
contrario, così le sue punte di freccia e i suoi tratteggi lo seguono.

Ogni modifica è un passo da annullare, e Annulla e Ripeti nominano il passo
che compiono; si conservano fino a 1000 passi, meno quando manca memoria.
Copia mette la selezione negli appunti come SVG, e Incolla aggiunge ciò che
contengono gli appunti al livello in mano, al suo posto sulla pagina,
selezionato.

File ▸ Inserisci immagine (`Ctrl+Shift+P`) chiede un'immagine — un file PNG,
JPEG, GIF, BMP, TIFF, WebP, di icona di Windows o OpenRaster, o un file di
sprite di RISC OS, la prima pagina o il primo sprite di uno che ne contiene
diversi — e la posa nel livello in mano come un solo passo, selezionata e
chiamata come il suo file, in mezzo a ciò che mostra la tela: alla sua
dimensione, o ridotta a quattro quinti della tela dove è più grande. Il file
viene solo letto, mai cambiato. Un'immagine si sposta, si scala, ruota, si
ritaglia e si copia come ogni oggetto; non ha stile né nodi propri, e dove le
forme si uniscono, si tagliano o si dividono vi partecipa come il suo
rettangolo. Si salva con il disegno: l'SVG la incorpora, una copia PDF o EPS
la disegna e un file Draw la tiene come sprite o come JPEG. Le immagini di un
disegno contengono insieme al massimo 268 milioni di pixel, ciascuna contata
una volta per quanto spesso sia mostrata; un'immagine che le porterebbe oltre
viene rifiutata, e la banda di stato dice perché. Un'immagine trascinata dal
gestore di file o dal desktop e lasciata sulla tela viene inserita allo stesso
modo, centrata dove è stata lasciata; ogni altro file lasciato sulla finestra
si apre, o viene rifiutato, come uno lasciato sull'icona di Draw.

Impostazioni, nel menu della barra delle icone, apre la finestra delle
impostazioni di Draw: lo strumento con cui comincia una finestra nuova, e se
mostra la griglia, aggancia e disegna solo i contorni; la carta,
l'orientamento, le unità e la griglia con cui comincia un disegno nuovo; il
riempimento e il tratto con cui si disegnano gli oggetti nuovi, ciascuno
nessuno o un colore, e lo spessore del tratto; di quanto spostano i tasti
freccia, un pixel se non impostato; a che distanza un duplicato sta da ciò che
copia; e quanto il puntatore deve avvicinarsi a una maniglia, a un nodo o a
una guida per prenderli. Una modifica vale subito e resta per la volta
successiva; Ripristina predefiniti le rimette tutte.

Draw non detiene alcuna capacità sul file system. Modifica solo il file che
gli è stato consegnato: un file che l'utente può cambiare gli è consegnato
scrivibile e Salva lo riscrive, e ogni altro si apre in sola lettura e Salva
chiede dove salvarlo. I documenti, i disegni incollati dagli appunti e le
immagini inserite vengono letti in un processo di lavoro separato senza alcuna
portata, così che un file ostile non può toccare nulla di ciò che Draw
raggiunge.

Premere il pulsante secondario (destro) del mouse in qualunque punto della
finestra apre il suo menu: File, Pagina, Modifica, Oggetto, Tracciato,
Livello, Visualizza e Strumenti, ognuno un sottomenu. La finestra non ha una
barra dei menu. Chiudere una finestra, o uscire, con modifiche non salvate
chiede prima. Chiudere una finestra con il pulsante secondario sul suo
pulsante di chiusura mostra anche il suo file nel gestore di file, nella
cartella che lo contiene.

* `Ctrl+N` — una finestra nuova; `Ctrl+O` — aprire un file; `Ctrl+Shift+P` — inserire un'immagine
* `Ctrl+S` — salvare; `Ctrl+Shift+S` — salvare con nome; `Ctrl+Shift+E` — salvare una copia
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
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `G`, `I`, `Z`, `H` — gli strumenti, in ordine
* tasti freccia — spostare la selezione quanto dicono le impostazioni, un pixel se non impostato; con `Shift`, dieci volte tanto
* `Space` — premuto, trascinare la tela con qualunque strumento
* `Ctrl` + rotella — ingrandire attorno al puntatore; `Shift` + rotella — scorrere in orizzontale

## OPTIONS

`-h`, `-?`, `--help`
: Scrive questa guida sull'output standard ed esce.

## EXIT STATUS

Zero dopo Esci. Diverso da zero quando il canale della finestra, la casella
degli eventi o la sessione del desktop è stata rifiutata; il motivo è indicato
sull'output di errore standard.
