## NAME

Paint — editor grafico di immagini e sprite

## SYNOPSIS

`Paint`

## DESCRIPTION

Dipinge e modifica immagini in una finestra del desktop, pixel per pixel o
con pennelli e forme. Avviato con un documento — dal gestore dei file, dal
desktop o lasciando cadere un file sulla sua icona nella barra delle icone —
apre una finestra su di esso. Avviato da solo apre una nuova immagine
bianca. Ogni documento è una finestra dell'unico programma; chiudere
l'ultima lo lascia nella barra delle icone, e la riga Esci del suo menu
dell'icona lo termina.

Apre ogni formato di immagine che il sistema legge: PNG, JPEG, GIF, BMP,
TIFF, WebP, icone di Windows, file di sprite di RISC OS e OpenRaster. Scrive
PNG, JPEG, GIF, BMP, TIFF, file di sprite e OpenRaster; un'immagine letta da
un altro formato, o da un file che contiene più della sua immagine, come un
profilo di colore, viene salvata come nuovo file. Nuova immagine chiede per
prima cosa a quale formato è destinata l'immagine e offre i colori che quel
formato contiene. Salva con nome chiede il formato e le sue impostazioni —
la qualità di un JPEG, se un GIF è interlacciato, la compressione di un TIFF
— e dice che cosa il formato non può conservare, prima di chiedere dove. Una
tavolozza viene conservata ovunque il formato ne contenga una, e così la
densità di un'immagine.

Un'immagine può essere fatta di livelli, il più basso per primo, ciascuno
con un nome, un'opacità e se è visibile; si dipinge su un livello alla
volta, e la finestra li mostra sovrapposti. OpenRaster conserva i livelli;
ogni altro formato li riceve sovrapposti. Il menu Livelli aggiunge, copia,
elimina, alza e abbassa livelli, unisce un livello a quello sottostante e
appiattisce l'immagine, e le sue Proprietà del livello rinominano un livello
e fissano quanto se ne vede. Regolazioni, riempimenti e tratti cambiano il
livello su cui si dipinge; rotazioni, ribaltamenti, ridimensionamenti e
ritagli cambiano ogni livello. Un'immagine con tavolozza ha un solo livello.

Un file di sprite contiene un numero qualsiasi di sprite, ciascuno con nome,
modo dello schermo, tavolozza e maschera. Ogni profondità viene modificata
come è memorizzata: 2, 4, 16 e 256 colori e milioni di colori. Uno sprite
senza tavolozza propria mostra i colori del desktop di RISC OS — con 16
colori il colore n è il colore Wimp n; con 2 colori i colori Wimp 0 e 7;
con 4 colori i colori Wimp 0, 2, 4 e 7; con 256 colori la disposizione delle
tinte di RISC OS — mai una tavolozza da PC. Uno sprite i cui pixel sono più
alti che larghi, come nel modo 12, è mostrato così. Uno sprite che questo
editor non sa leggere, come uno CMYK, è conservato esattamente com'era e
riscritto senza modifiche. Il menu Sprite va agli sprite, ne aggiunge, ne
copia, li rinomina, li elimina e li riordina. Un TIFF contiene un numero
qualsiasi di pagine, e per esso il menu Pagine va alle pagine, ne aggiunge,
ne copia, le elimina e le riordina.

Il pulsante principale (sinistro) dipinge con il colore principale e quello
centrale con il colore secondario; tenendo premuto Alt si preleva invece un
colore, come lo mostrano i livelli. Gli strumenti nella cassetta a sinistra
sono selezione, matita, pennello, aerografo, gomma, clone, riempimento,
sfumatura, contagocce, testo, linea, rettangolo, ellisse, poligono, ritaglio,
mano e zoom. La barra in alto nomina lo strumento in uso e contiene le sue
impostazioni — la dimensione, la durezza, l'opacità, il flusso e la
spaziatura di un pennello, la tolleranza di un riempimento, la forma di una
sfumatura, la dimensione del testo, gli angoli di un rettangolo — digitate
o regolate con i tasti freccia, e i pulsanti che ingrandiscono e mostrano la
griglia dei pixel; la striscia della tavolozza sotto l'immagine contiene la
tavolozza dell'immagine, o i colori del desktop. L'aerografo continua a
spruzzare finché è tenuto fermo. Tenendo premuto Maiusc si disegna un
quadrato, un cerchio o una linea a un multiplo di 45 gradi.

Lo strumento di selezione delimita un rettangolo, un'ellisse, un lazo a mano
libera, un poligono cliccato angolo per angolo o, con la bacchetta magica, i
pixel uniti a uno attraverso colori simili. Maiusc aggiunge alla selezione,
Alt ne toglie, ed entrambi tengono solo ciò che hanno in comune; Sfuma
ammorbidisce il suo bordo. Finché c'è una selezione, ogni strumento,
riempimento e regolazione vi è vincolato. Trascinare al suo interno la
solleva e la sposta: resta sospesa finché non viene posata, e spostarla è
una sola modifica da annullare. Le immagini copiate passano dagli appunti
come PNG, e ciò che si incolla resta sospeso finché non viene posato.

Lo strumento clone dipinge ciò che si trova altrove nell'immagine: Alt-clic
dove copiare, poi dipingere. Lo strumento sfumatura fonde il colore
principale nel secondario lungo un trascinamento, a bande o ad anelli. Lo
strumento testo pone le parole digitate dove si clicca; Invio inizia una
nuova riga, un altro clic o un altro strumento le posa, ed Esc le scarta.
Gli angoli dello strumento poligono si cliccano a turno, e un clic sul primo
o Invio lo chiude. Lo strumento ritaglio segna la parte da tenere, le sue
maniglie ne spostano i bordi, e Invio ritaglia. La mano trascina l'immagine
nella finestra, come Spazio con qualsiasi strumento; lo zoom ingrandisce con
un clic, o con Alt riduce, e un riquadro trascinato riempie la finestra.

Il menu Regolazioni cambia luminosità e contrasto, tonalità e saturazione e
livelli, posterizza, applica una soglia, desatura, sfoca, aumenta la
nitidezza, pixella, aggiunge rumore e trova i bordi; ognuna si vede
sull'immagine mentre le sue impostazioni si muovono e resta solo se
applicata. Su un'immagine con tavolozza una regolazione cambia la sua
tavolozza, e quelle che richiedono i colori vicini non sono offerte.

Il pannello dei colori a destra contiene i colori principale e secondario e
un selettore di colore per quello dei due che è scelto: un clic su un colore
lo sceglie, poi lo si regola per tonalità, saturazione e valore, per rosso,
verde e blu, per la sua scrittura esadecimale e, dove l'immagine contiene
trasparenza, per la sua opacità. Il colore che aveva sta accanto, e un clic
lo riprende. In un'immagine con tavolozza i colori sono le sue voci, quindi
il selettore modifica la tavolozza, e ogni modifica è una sola da annullare.

Il programma non ha alcun permesso sul file system. Modifica soltanto il
file che gli è stato consegnato. Un file che l'utente può cambiare viene
consegnato in scrittura, e Salva lo riscrive; ogni altro è di sola lettura,
e Salva chiede dove salvarne una copia. Le immagini, e quelle incollate
dagli appunti, sono decodificate in un processo di lavoro separato senza
alcuna portata, e ogni documento ne riceve uno nuovo: un file ostile non può
toccare nulla di ciò che il programma può raggiungere.

Premendo il pulsante secondario (destro) del mouse in qualunque punto della
finestra si apre il suo menu: Taglia, Copia, Incolla, Seleziona tutto e
Deseleziona, poi File, Modifica, Immagine, Livelli, Colori, Regolazioni,
Sprite o Pagine, Visualizza e Strumenti, ciascuno con il proprio sottomenu.
La finestra non ha una barra dei menu. Chiudere una finestra o uscire con
modifiche non salvate chiede prima conferma.

* `Ctrl+N` — una nuova immagine; `Ctrl+O` — aprire un file
* `Ctrl+S` — salvare; `Ctrl+Shift+S` — salvare con nome
* `Ctrl+W` — chiudere la finestra
* `Ctrl+Z` — annullare; `Ctrl+Shift+Z` o `Ctrl+Y` — ripetere
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — tagliare, copiare, incollare
* `Ctrl+A` — selezionare tutto; `Ctrl+D` — deselezionare
* `Enter` — posare una selezione sospesa, chiudere un poligono o ritagliare; `Escape` — tornare indietro
* `Delete` — cancellare la selezione; `Alt+Backspace` — riempirla con il colore principale
* `Ctrl+Shift+X` — ritagliare alla selezione
* `Ctrl+R` — ridimensionare; `Ctrl+Shift+R` — dimensione della tela
* `Ctrl+[` / `Ctrl+]` — ruotare a sinistra o a destra
* `Ctrl+I` — invertire i colori
* `Ctrl+Shift+N` — un nuovo livello; `Ctrl+E` — unire in basso; `Ctrl+Shift+E` — appiattire
* `Ctrl+Page Up` / `Ctrl+Page Down` — dipingere sul livello sopra o sotto
* `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — alzare o abbassare il livello
* `S`, `P`, `B`, `A`, `E`, `C`, `F`, `D`, `I`, `T`, `L`, `R`, `O`, `Y`, `K`, `H`, `Z` — gli strumenti, in ordine
* `Space` — tenuto premuto, trascinare l'immagine con qualsiasi strumento
* `X` — scambiare i colori principale e secondario
* `Tab` — tra le impostazioni dello strumento, la striscia della tavolozza e il pannello dei colori; `Shift+Tab` — all'indietro; `Escape` — di nuovo all'immagine
* `+` / `-` — ingrandire o ridurre; `1` — dimensione reale; `Ctrl+0` — adattare
* `Ctrl` + rotella — ingrandire o ridurre attorno al puntatore
* Pizzicare con due dita — ingrandire o ridurre con continuità; su un touchscreen l'immagine segue le dita
* `G` — mostrare o nascondere la griglia tra i pixel
* `Page Up` / `Page Down` — lo sprite o la pagina precedente o successiva
* tasti freccia — spostare una selezione sospesa di un pixel; con `Shift`, di dieci; nella striscia della tavolozza, scorrerne i colori

## OPTIONS

`-h`, `-?`, `--help`
: Scrive questo aiuto sullo standard output ed esce.

## EXIT STATUS

Zero dopo Esci. Diverso da zero quando il canale della finestra, la casella
degli eventi o la sessione del desktop è stato rifiutato; il motivo è
indicato sullo standard error.
