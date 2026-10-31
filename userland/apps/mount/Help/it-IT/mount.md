## NAME

mount — elencare i filesystem montati o montare un'immagine disco

## SYNOPSIS

`mount`

`mount [option...] image [target]`

## DESCRIPTION

Senza operandi, elenca i filesystem montati, una riga
`source on target type fstype (options)` per ogni posizione in cui è
mostrato un volume. `-t` elenca allora solo i tipi che la sua lista
separata da virgole nomina; una lista che comincia con `no` elenca ogni
tipo che non nomina.

Con un operando `image`, monta il volume contenuto in quel file
immagine disco: un disco ottico ISO 9660 o UDF (`.iso`), un dischetto
Acorn ADFS o AmigaDOS (`.adf`, `.adl`, `.adm`, `.ads`), una partizione
di disco rigido AmigaDOS (`.hdf`) o un volume FAT32. Ciò che
l'immagine racchiude è riconosciuto dal suo contenuto, mai dal suo
nome. Il volume è montato solo per voi, in `/Storage/name`, con un
nome tratto dall'etichetta del volume o, in sua assenza, dal nome del
file dell'immagine; nessun altro può aprirlo. È montato in lettura e
scrittura quando potete scrivere l'immagine e il suo formato consente
la scrittura, altrimenti in sola lettura, cosa di cui `mount` avvisa.
Su un volume montato da un'immagine non si eseguono mai programmi.

Un `target` della forma `/Storage/name` monta il volume sotto quel
nome. Qualsiasi altro `target`, una directory vuota di vostra
proprietà sotto `/Users`, mostra il volume anche lì.

Il volume resta montato finché non lo scollegate con `unmount` o non
termina la vostra sessione di accesso. Un'immagine che contiene un
disco partizionato, uno dei dischi di un array RAID o un filesystem
i cui proprietari registrati sono sempre rispettati (ext2, ext3, ext4,
ARXFS) non viene montata, e `mount` ne dice il motivo.

## OPTIONS

- `-r, --read-only` — montare il volume in sola lettura (come `-o ro`).
- `-w, --rw, --read-write` — montare il volume in lettura e scrittura,
  oppure non montarlo affatto: un'immagine che non potete scrivere,
  o il cui filesystem non consente la scrittura, non viene montata.
  Vince l'ultima tra `-r`, `-w`, `-o ro` e `-o rw`.
- `-t, --types <type>` — il filesystem che l'immagine deve contenere,
  con il nome che gli dà l'elenco dei montaggi (`iso9660`, `udf`,
  `adfs`, `affs`, `fat32`), oppure `auto` per qualunque filesystem
  contenga. Un'immagine che ne contiene un altro, o per cui si indica
  un tipo che nessuna immagine disco contiene, non viene montata.
- `-o, --options <list>` — un elenco separato da virgole di `ro`,
  `rw` (in lettura e scrittura quando è possibile, il comportamento
  predefinito), `noexec`, `loop` e `name=name`, il nome che il volume
  deve assumere. `noexec` e `loop` sono il modo in cui un'immagine
  viene montata in ogni caso.
- `-v, --verbose` — indicare dove è stato montato il volume.
- `-l, --show-labels` — accettato per l'elenco, che non ha etichette
  da aggiungere.
- `-h, -?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `mount` — elencare i filesystem montati.
- `mount Workbench.adf` — montare l'immagine di un dischetto Amiga, in
  `/Storage/workbench` se è questa la sua etichetta.
- `mount -r disc.iso /Storage/disc` — montare in sola lettura
  l'immagine di un disco ottico come `disc`.
- `mount -v -t adfs Elite.adl` — montare l'immagine di un dischetto
  Acorn, indicando dove.

## EXIT STATUS

- `0` — l'elenco è stato scritto, o il volume è stato montato (o la
  guida breve è stata scritta).
- `1` — la riga di comando non è stata compresa.
- `2` — non è stato possibile leggere l'elenco dei montaggi, o
  scrivere sul terminale.
- `32` — l'immagine non è stata montata.

## ENVIRONMENT

- `LANG` — la localizzazione preferita per la guida breve
  (un'etichetta BCP-47 come `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
