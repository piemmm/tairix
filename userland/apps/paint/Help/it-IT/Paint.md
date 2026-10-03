## NAME

Paint — editor grafico di immagini e sprite

## SYNOPSIS

`Paint`

## DESCRIPTION

Dipinge e modifica immagini in una finestra del desktop, pixel per pixel o
con pennelli e forme. Avviato con un documento — dal gestore di file, dal
desktop o trascinando un file sulla sua icona nella barra delle icone —
apre una finestra su di esso. Avviato da solo, apre una nuova immagine
bianca. Ogni documento è una finestra dell'unico programma; chiudere
l'ultima lo lascia nella barra delle icone, e la riga Esci del suo menu
dell'icona lo termina.

Apre ogni formato di immagine che il sistema legge: PNG, JPEG, GIF, BMP,
TIFF, WebP, icone di Windows e file di sprite di RISC OS. Scrive PNG, JPEG e
file di sprite; un'immagine letta da qualsiasi altro formato viene salvata
come nuovo file. Un PNG mantiene la sua tavolozza, e un JPEG viene scritto
alla qualità impostata con Qualità JPEG nel menu File.

Un file di sprite contiene un numero qualsiasi di sprite, ciascuno con il
suo nome, modo di schermo, tavolozza e maschera. Ogni profondità viene
modificata così come è memorizzata: 2, 4, 16 e 256 colori e milioni di
colori. Uno sprite senza tavolozza propria mostra i colori del desktop di
RISC OS — a 16 colori, il colore n è il colore Wimp n; a 2 colori, i colori
Wimp 0 e 7; a 4 colori, i colori Wimp 0, 2, 4 e 7; a 256 colori, la
disposizione delle tinte di RISC OS — mai una tavolozza da PC. Uno sprite i
cui pixel sono più alti che larghi, come nel modo 12, viene mostrato così.
Uno sprite che questo editor non sa leggere, per esempio uno CMYK, viene
conservato esattamente com'era e risalvato senza modifiche. Il menu Sprite
va a uno sprite, aggiunge, copia, rinomina, elimina e riordina gli sprite.

Il pulsante principale (sinistro) dipinge con il colore principale e il
pulsante centrale con il colore secondario; tenendo premuto Alt si preleva
invece un colore. Gli strumenti sono selezione, matita, pennello,
aerografo, gomma, riempimento, contagocce, linea, rettangolo ed ellisse; il
pannello accanto all'immagine contiene la tavolozza dell'immagine o i
colori del desktop e le impostazioni dello strumento in uso. Il pannello
dei colori a destra contiene i colori principale e secondario e un
selettore di colore per quello dei due scelto: un clic su un colore lo
sceglie, poi lo si imposta per tonalità, saturazione e valore, per rosso,
verde e blu, con la sua scrittura esadecimale e, dove l'immagine ammette la
trasparenza, con la sua opacità. Accanto resta il colore che aveva, e un
clic lo ripristina. In un'immagine con tavolozza i colori ne sono le voci,
quindi il selettore modifica la tavolozza, e ogni modifica è un passo da
annullare. Tenendo premuto
Maiusc si disegna un quadrato, un cerchio o una linea a multipli di 45
gradi.

Con lo strumento di selezione, trascinare per delimitare una parte
dell'immagine, poi trascinare la selezione per spostarla; resta sospesa
finché non viene posata, e spostarla è un'unica modifica da annullare. Le
immagini copiate passano per gli appunti come PNG, e ciò che viene incollato
resta sospeso finché non viene posato.

Il programma non detiene alcuna capacità sul file system. Modifica solo il
file che gli è stato consegnato. Un file che l'utente può cambiare viene
consegnato in scrittura, e Salva lo riscrive; qualsiasi altro è di sola
lettura, e Salva chiede dove salvarne una copia. Le immagini, anche quelle
incollate dagli appunti, vengono decodificate in un processo di lavoro
separato privo di qualsiasi accesso, e ogni documento ne riceve uno nuovo:
un file ostile non può toccare nulla di ciò che il programma può toccare.

Premere il pulsante secondario (destro) del mouse in un punto qualsiasi
della finestra apre il suo menu: Taglia, Copia, Incolla, Seleziona tutto e
Deseleziona, poi File, Modifica, Immagine, Colori, Sprite, Vista e
Strumenti, ciascuno con il suo sottomenu. La finestra non ha una barra dei
menu. Chiudere una finestra o uscire con modifiche non salvate chiede prima
conferma.

* `Ctrl+N` — una nuova immagine; `Ctrl+O` — aprire un file
* `Ctrl+S` — salvare; `Ctrl+Shift+S` — salvare con nome
* `Ctrl+W` — chiudere la finestra
* `Ctrl+Z` — annullare; `Ctrl+Shift+Z` o `Ctrl+Y` — ripetere
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — tagliare, copiare, incollare
* `Ctrl+A` — selezionare tutto; `Ctrl+D` — deselezionare
* `Enter` — posare una selezione sospesa; `Escape` — rimetterla a posto
* `Delete` — cancellare la selezione
* `Ctrl+Shift+X` — ritagliare alla selezione
* `Ctrl+R` — ridimensionare; `Ctrl+Shift+R` — dimensione della tela
* `Ctrl+[` / `Ctrl+]` — ruotare a sinistra o a destra
* `Ctrl+I` — invertire i colori
* `S`, `P`, `B`, `A`, `E`, `F`, `I`, `L`, `R`, `O` — gli strumenti, in ordine
* `X` — scambiare i colori principale e secondario
* `Tab` — nel pannello dei colori e tra le sue parti; `Escape` — di nuovo all'immagine
* `+` / `-` — ingrandire o ridurre; `1` — dimensione reale; `Ctrl+0` — adattare
* `Ctrl` + rotella — ingrandire o ridurre attorno al puntatore
* Pizzicare con due dita — ingrandire o ridurre in modo continuo; su uno schermo tattile l'immagine segue le dita
* `G` — mostrare o nascondere la griglia tra i pixel
* `Page Up` / `Page Down` — lo sprite precedente o successivo
* tasti freccia — spostare una selezione sospesa di un pixel; con `Shift`, di dieci

## OPTIONS

`-h`, `-?`, `--help`
: Scrive questo aiuto sullo standard output ed esce.

## EXIT STATUS

Zero dopo Esci. Diverso da zero quando il canale della finestra, la casella
degli eventi o la sessione del desktop sono stati rifiutati; il motivo è
indicato sullo standard error.
