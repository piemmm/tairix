## NAME

wintersun — percorrere un mondo generato proceduralmente

## SYNOPSIS

`wintersun [--seed SEED | --reference-scene]`

## DESCRIPTION

Apre una finestra del desktop su un mondo generato: una veduta dall'alto di un
terreno che la macchina sintetizza invece di distribuire, illuminato da un
sole basso che allunga le ombre lungo ogni pendio.

Il mondo va da una calotta di ghiaccio nell'estremo nord fino alla foresta
pluviale oltre l'equatore. Il suo clima segue la latitudine, i venti dominanti e
le montagne che si frappongono loro, così un deserto si stende dove la pioggia
non arriva e una torbiera dove l'acqua non riesce a defluire. Ogni mondo è il
frutto di un solo numero, il suo seme: lo stesso seme apre lo stesso mondo su
ogni macchina.

Nulla di questo mondo è memorizzato come immagine. Ogni materiale di cui è fatto
il suolo — ghiaccio, sabbia di duna, erba secca, sottobosco, granito — è una
manciata di numeri che il client trasforma in texture mentre disegna, così il
mondo appare identico su ogni macchina e occupa quasi nulla su disco. Le strade
si consumano dentro ciò che attraversano invece di posarvisi sopra.

Il terreno non ancora generato è disegnato come il vuoto che è e si riempie
man mano che arriva. Il client disegna quello che ha invece di fermarsi ad
aspettare, così la finestra continua a rispondere mentre il mondo recupera.

I tasti freccia o `W`, `A`, `S`, `D` fanno camminare. Due tasti tenuti insieme
percorrono la diagonale fra loro alla stessa velocità, e i tasti opposti si
annullano. La veduta vi segue e si ferma al bordo del mondo invece di
scivolarne fuori. I pendii troppo ripidi e l'acqua troppo profonda vi
deviano.

`+` e `-` avvicinano e allontanano la veduta, in cinque passi, da una cella
del mondo larga otto pixel a una larga centoventotto.

`F11` porta la finestra a schermo intero e poi la riporta com'era: una
finestra ingrandita torna ingrandita. `Esc` la ripristina. `Q` esce.

Ogni dettaglio è disegnato al massimo livello finché non si sceglie
altrimenti. La voce *Settings…* del menu del gioco nella barra delle icone
apre la sua finestra delle impostazioni, dove la qualità è *Ultra*, ogni
dettaglio al massimo; *Basic*, ogni dettaglio al minimo, alla piena dimensione
della finestra; *Custom*, la propria scelta di illuminazione, ombre, trama del
terreno e scala di rendering, ciascuna con il proprio cursore; oppure *Auto*.
Spostare un cursore rende la scelta *Custom*. La scelta viene conservata per
la prossima partita.

In *Auto* il client riduce il dettaglio quando i fotogrammi arrivano in
ritardo da un po' — prima l'illuminazione, poi le ombre, poi la scala di
rendering — e lo restituisce, un passo alla volta, man mano che si riprendono.
Giudica nell'arco di secondi anziché di singoli fotogrammi, quindi un momento
di altro lavoro sulla macchina non costa nulla e una finestra più grande non
lo porta al minimo, e non disegna mai le figure troppo piccole per essere
leggibili.

Una finestra più grande di quanto il renderer software possa riempire è
disegnata al massimo a 2560×1440 e ingrandita fino alla finestra.

## OPTIONS

- `-h, -?, --help` — mostrare la guida breve di questo comando.
- `--seed SEED, --seed=SEED` — aprire il mondo indicato da SEED, un numero
  intero da 0 a 18446744073709551615. Senza questa opzione il gioco estrae un
  seme nuovo e lo riporta sul flusso di informazioni standard, il descrittore 3,
  così che lo stesso mondo possa essere riaperto. Non insieme a
  `--reference-scene`, che è un unico mondo fisso.
- `--reference-scene` — disegnare la scena di riferimento fissa e tenerla
  ferma: un solo mondo, gli stessi personaggi e lo stesso istante, identici su
  ogni macchina, così che un'immagine della finestra possa essere confrontata
  con una disegnata altrove. `F11` ed `Esc` cambiano ancora le dimensioni
  della finestra; nient'altro si muove.

## EXIT STATUS

`0` quando si esce. Uno stato diverso da zero indica il motivo sull'uscita di
errore: il mondo non ha potuto essere generato, la finestra non ha potuto
essere aperta, oppure il canale degli eventi della sessione è andato perso.

- `2` — la riga di comando non è stata compresa.
- `87` — la scena di riferimento non ha potuto essere disegnata.

## SEE ALSO

`sapper`
