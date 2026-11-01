## NAME

unmount — scollegare un volume montato

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Mette fuori servizio il volume montato sotto `name`: il filesystem e
il dispositivo vengono svuotati, il montaggio sotto `/Storage` viene
rimosso e la radice durevole `id::` del volume viene revocata. `name`
è il nome di catalogo del volume (`usb1`), una directory su cui è
montato (`/Storage/usb1`, o quella a cui `mount immagine directory`
l'ha collegato), o il nome del file dell'immagine disco da cui è stato
montato (`Workbench.adf`), confrontato con gli elenchi dei montaggi e
delle viste dell'API di informazioni di sistema; un nome di file
condiviso con l'immagine di un altro utente indica la vostra. Un nome di
file che potrebbe indicare più di un volume, o di cui l'elenco conserva
solo l'inizio, non ne indica alcuno: indicate allora il volume con il
punto in cui è montato.
Un percorso può essere relativo o terminare con una barra: viene
risolto come `mount` ne risolve uno. Il volume privato di un altro
utente non mostra né il nome del file né le directory a cui è
collegato, quindi lo si indica con il punto in cui è montato in
`/Storage`.

Un volume il cui dispositivo è stato rimosso con scritture non ancora
confermate resta visibile come `unavailable-dirty` nell'elenco dei
montaggi, e un `unmount` semplice rifiuta: i dati trattenuti sono
conservati per un reinserimento verificato. Lo stesso vale per un volume
reinserito di cui non si è potuto dimostrare che sia rimasto invariato
(`recovery-conflict`), il quale trattiene ancora quei dati, e per uno di
cui è stato impossibile conservare le scritture non confermate
(`unavailable-lost`), perché la loro perdita sia riconosciuta anziché
passata sotto silenzio. Anche un volume su cui restano file aperti, o
che un programma ha come directory di lavoro, rifiuta un `unmount`
semplice. `--force` è l'uscita deliberata — i file ancora aperti sul
volume vengono tagliati fuori, un programma che vi lavora resta in una
directory che non esiste più, i dati trattenuti vengono scartati, il
volume viene rimosso e ciò a cui si è rinunciato è registrato nel
registro di controllo. Su un volume sano `--force` svuota e scollega
comunque in modo pulito; nulla viene scartato quando una conferma pulita
è possibile.

Potete scollegare un'immagine disco o una condivisione di rete che
avete montato voi stessi; qualsiasi altro volume richiede l'autorità
di montaggio (`CAP_FS_MOUNT`). Il kernel la verifica e registra ogni
decisione. I volumi di avvio permanenti e i collegamenti di vista del
sistema non sono scollegabili.

## OPTIONS

- `-f, --force` — smontaggio forzato: rimuovere il volume anche mentre
  vi sono file aperti, un programma vi lavora o i suoi dati non ancora
  confermati non possono esserlo, tagliando fuori i file aperti e
  scartando i dati trattenuti.
- `-h, -?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `unmount usb1` — scollegare in modo pulito il volume montato come
  `usb1`.
- `unmount /Storage/usb1` — lo stesso, indicato dal punto di
  montaggio.
- `unmount Workbench.adf` — scollegare l'immagine disco montata da
  `Workbench.adf`.
- `unmount --force usb1` — rimuovere un volume non disponibile
  scartando i suoi dati trattenuti.

## EXIT STATUS

- `0` — il volume è stato scollegato (o la guida breve è stata
  scritta).
- `1` — la riga di comando non è stata compresa, o il chiamante non può
  scollegare il volume.
- `2` — il servizio di informazioni di sistema, o la scrittura sul
  terminale, non è riuscito.
- `32` — il volume non era montato o non è scollegabile, il percorso che
  lo indica non è stato risolto, o il suo scollegamento non è riuscito.

## ENVIRONMENT

- `LANG` — la localizzazione preferita per la guida breve
  (un'etichetta BCP-47 come `it-IT`).

## SEE ALSO

- `mount`
- `df`
- `man`
