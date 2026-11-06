## NAME

label — mostrare o cambiare il titolo di un volume

## SYNOPSIS

`label [option...] name [title]`

## DESCRIPTION

Mostra il titolo del volume indicato da `name` oppure, se è dato
`title`, lo imposta. Il titolo di un volume è l'etichetta che registra
il suo file system: il nome con cui è mostrato e quello che riceve in
`/Storage` al successivo montaggio. `name` è il nome di catalogo del
volume (`usb1`), una directory in cui è montato (`/Storage/usb1`), il
nome del file dell'immagine disco da cui è stato montato
(`Workbench.adf`) o la sua radice durevole (`id::…`), indicati come
per `unmount`. Un `title` vuoto cancella il titolo dove il formato del
volume lo consente.

Finché il volume resta montato, il suo nome in `/Storage` non cambia,
quindi nulla di ciò che lo indica lì smette di funzionare; il nuovo
titolo lo nomina dal successivo montaggio. La sua identità e quella dei
suoi file restano le stesse.

Ogni formato scrive un titolo a modo suo, e un titolo che non può
memorizzare viene rifiutato anziché accorciato. Un volume FAT accetta
fino a undici lettere maiuscole, cifre, spazi e la punteggiatura
ammessa da un nome di file breve, senza iniziare né finire con uno
spazio; un volume ext2, ext3 o ext4 fino a sedici byte; un volume ADFS
fino a dieci caratteri; un volume AmigaDOS da uno a trenta; un volume
UDF fino a 126; e un volume ARXFS fino a 252 byte, a meno che non sia
cifrato. Un disco masterizzato con un titolo, un archivio e una
condivisione di rete non ne accettano uno nuovo.

Un'immagine disco che hai montato tu stesso puoi rinominarla tu. Un
disco rimovibile — una chiavetta USB, una scheda — puoi rinominarlo
mentre usi il computer stesso, se puoi modificare il contenuto della sua
cartella principale; ogni altro volume richiede l'autorità di montaggio
(`CAP_FS_MOUNT`), di cui dispone un amministratore. Il kernel lo
verifica e annota ogni decisione nel registro di controllo.

## OPTIONS

- `-h, -?, --help` — mostra la guida breve di questo comando.
- `--version` — mostra il nome e la versione di questo comando.

## EXAMPLES

- `label usb1` — mostra il titolo del volume montato come `usb1`.
- `label usb1 HOLIDAY` — gli dà il titolo `HOLIDAY`.
- `label /Storage/backup 'Backup 2026'` — dà un titolo al volume
  montato su `/Storage/backup`.
- `label usb1 ''` — cancella il suo titolo.

## EXIT STATUS

- `0` — il titolo è stato mostrato o impostato (o è stata scritta la
  guida o la versione).
- `1` — la riga di comando non è stata compresa, il nome non indicava
  alcun volume, il titolo non era tale, o il volume lo ha rifiutato.
- `2` — il servizio di informazioni di sistema, o la scrittura sul
  terminale, non è riuscito.

## ENVIRONMENT

- `LANG` — la localizzazione preferita per la guida breve (un tag
  BCP-47 come `fr-FR`).

## SEE ALSO

- `mount`
- `unmount`
- `df`
