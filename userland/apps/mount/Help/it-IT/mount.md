## NAME

mount — elencare i filesystem montati o montare un'immagine disco o un archivio

## SYNOPSIS

`mount`

`mount [option...] image [target]`

`mount --list-held`

`mount --replay-held name`

`mount --discard-held name`

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

Un `target` della forma `/Storage/name` monta il volume sotto quel nome.
Qualsiasi altro `target`, una directory vuota di vostra proprietà sotto
`/Users` o su un volume in `/Storage`, mostra il volume anche lì; una
directory su un volume FAT32 o ADFS non può mostrarlo.

Un archivio viene montato allo stesso modo, e i suoi elementi diventano
i file e le cartelle del volume: un archivio zip (`.zip`), un archivio
tar, non compresso o compresso con gzip o bzip2 (`.tar`, `.tar.gz`,
`.tgz`, `.tar.bz2`), un singolo file compresso con gzip o bzip2 (`.gz`,
`.bz2`), un archivio RISC OS Spark o ArcFS (`.spk`, `.arc`) o un
archivio RAR (`.rar`). I tipi di file e gli attributi RISC OS
accompagnano ciascun elemento. Le modifiche vengono salvate sostituendo
il file dell'archivio, perciò un archivio è montato in lettura e
scrittura solo quando potete scrivere sia il file sia la cartella che lo
contiene. Un archivio RAR è sempre montato in sola lettura. Un archivio
suddiviso in più volumi, o con i nomi degli elementi cifrati, non viene
montato, e un elemento cifrato non può essere letto.

Il volume resta montato finché non lo scollegate con `unmount` o non
termina la vostra sessione di accesso. Un'immagine che contiene un
disco partizionato, uno dei dischi di un array RAID o un filesystem
i cui proprietari registrati sono sempre rispettati (ext2, ext3, ext4,
ARXFS) non viene montata, e `mount` ne dice il motivo.

Un disco reinserito mentre conteneva ancora modifiche non ancora
scritte su di esso viene montato in sola lettura, con le modifiche
conservate, quando il suo dispositivo non può mostrare che si tratta
dello stesso disco e non di una copia; l'elenco lo contrassegna con
`[replay-unconfirmed]`, e `--list-held` elenca solo questi volumi.
`--replay-held` vi scrive le modifiche, se il suo contenuto lo mostra
invariato da allora; `--discard-held` le scarta e prende il disco così
com'è. In entrambi i casi il volume viene montato di nuovo, in lettura
e scrittura dove possibile. Rispondere richiede l'autorità di montaggio
(`CAP_FS_MOUNT`); per un disco rimovibile, il desktop pone la domanda
anche all'utente davanti al computer.

## OPTIONS

- `-r, --read-only` — montare il volume in sola lettura (come `-o ro`).
- `-w, --rw, --read-write` — montare il volume in lettura e scrittura,
  oppure non montarlo affatto: un'immagine che non potete scrivere, o il
  cui filesystem non consente la scrittura, non viene montata. Un `-r`
  successivo revoca questa opzione; un `-o ro` o `-o rw` successivo no.
  L'ultima tra `-r`, `-w`, `-o ro` e `-o rw` decide se il volume è in
  sola lettura.
- `-t, --types <list>` — i filesystem, separati da virgole, uno dei
  quali l'immagine deve contenere, con i nomi che dà loro l'elenco dei
  montaggi (`iso9660`, `udf`, `adfs`, `affs`, `fat32`, `zip`, `tar`,
  `tar.gz`, `tar.bz2`, `gzip`, `bzip2`, `spark`, `arcfs`, `rar`), oppure
  `auto` per qualunque filesystem contenga; una lista che comincia con
  `no` nomina quelli che non deve contenere. Ciò che l'immagine
  racchiude è riconosciuto dal suo contenuto, perciò l'ordine non
  sceglie nulla: un disco ottico che contiene sia UDF sia ISO 9660 viene
  montato come UDF, a meno che il suo ISO 9660 non conservi record
  Rock Ridge o RISC OS. Un'immagine che non ne contiene nessuno, o una
  lista che non nomina alcun tipo che un'immagine disco possa contenere,
  non viene montata.
- `-o, --options <list>` — un elenco separato da virgole di `ro`,
  `rw` (in lettura e scrittura quando è possibile, il comportamento
  predefinito), `noexec`, `loop` e `name=name`, il nome che il volume
  deve assumere. `noexec` e `loop` sono il modo in cui un'immagine
  viene montata in ogni caso.
- `-v, --verbose` — indicare dove è stato montato il volume.
- `-l, --show-labels` — accettato per l'elenco, che non ha etichette
  da aggiungere.
- `--list-held` — elencare solo i volumi le cui modifiche attendono
  `--replay-held` o `--discard-held`.
- `--replay-held <name>` — scrivere sul suo disco le modifiche che un
  volume conserva, se il suo contenuto lo mostra ancora invariato;
  `name` è il nome del volume in `/Storage`, una directory in cui è
  mostrato o la sua radice `id::`.
- `--discard-held <name>` — scartare le modifiche che un volume
  conserva, prendendo il suo disco così com'è.
- `-h, -?, --help` — mostrare la guida breve di questo comando.

## EXAMPLES

- `mount` — elencare i filesystem montati.
- `mount Workbench.adf` — montare l'immagine di un dischetto Amiga, in
  `/Storage/workbench` se è questa la sua etichetta.
- `mount -r disc.iso /Storage/disc` — montare in sola lettura
  l'immagine di un disco ottico come `disc`.
- `mount -v -t adfs Elite.adl` — montare l'immagine di un dischetto
  Acorn, indicando dove.
- `mount Photos.zip` — montare un archivio zip, in lettura e scrittura
  quando potete scrivere l'archivio e la sua cartella.
- `mount -r backup.tar.gz /Storage/backup` — montare in sola lettura un
  archivio tar compresso come `backup`.
- `mount --list-held` — elencare i volumi le cui modifiche attendono
  una risposta.
- `mount --replay-held backup` — scrivere sul suo disco le modifiche
  che `/Storage/backup` conserva.

## EXIT STATUS

- `0` — l'elenco è stato scritto, il volume è stato montato, o le sue
  modifiche conservate sono state scritte o scartate (o la guida breve
  è stata scritta).
- `1` — la riga di comando non è stata compresa, o non potete
  rispondere per le modifiche del volume.
- `2` — non è stato possibile leggere l'elenco dei montaggi, o
  scrivere sul terminale.
- `32` — l'immagine non è stata montata, o le modifiche del volume non
  sono state né scritte né scartate.

## ENVIRONMENT

- `LANG` — la localizzazione preferita per la guida breve
  (un'etichetta BCP-47 come `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
