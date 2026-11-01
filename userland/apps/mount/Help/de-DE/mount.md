## NAME

mount — die eingehängten Dateisysteme auflisten oder ein Datenträgerabbild einhängen

## SYNOPSIS

`mount`

`mount [option...] image [target]`

## DESCRIPTION

Ohne Operanden werden die eingehängten Dateisysteme aufgelistet, eine
Zeile `source on target type fstype (options)` je Ort, an dem ein
Datenträger gezeigt wird. `-t` listet dann nur die Typen auf, die seine
kommagetrennte Liste nennt; eine Liste, die mit `no` beginnt, listet
jeden Typ, den sie nicht nennt.

Mit einem `image`-Operanden wird der Datenträger eingehängt, den diese
Abbilddatei enthält: eine ISO-9660- oder UDF-Disc (`.iso`), eine
Acorn-ADFS- oder AmigaDOS-Diskette (`.adf`, `.adl`, `.adm`, `.ads`),
eine AmigaDOS-Festplattenpartition (`.hdf`) oder ein FAT32-Datenträger.
Was das Abbild enthält, wird an seinem Inhalt erkannt, nie an seinem
Namen. Der Datenträger wird allein für Sie unter `/Storage/name`
eingehängt, benannt nach seiner Datenträgerbezeichnung oder ersatzweise
nach dem Dateinamen des Abbilds; niemand sonst darf ihn öffnen. Er wird
beschreibbar eingehängt, sofern Sie in das Abbild schreiben dürfen und
sein Format das Schreiben unterstützt, andernfalls schreibgeschützt,
worauf `mount` mit einer Warnung hinweist. Ein aus einem Abbild
eingehängter Datenträger führt nie Programme aus.

Ein `target` der Form `/Storage/name` hängt den Datenträger unter
diesem Namen ein. Jedes andere `target`, ein leeres eigenes Verzeichnis
unter `/Users` oder auf einem Datenträger in `/Storage`, zeigt den
Datenträger zusätzlich dort; ein Verzeichnis auf einem FAT32- oder
ADFS-Datenträger kann ihn nicht zeigen.

Der Datenträger bleibt eingehängt, bis Sie ihn mit `unmount` aushängen
oder Ihre Anmeldesitzung endet. Ein Abbild, das eine partitionierte
Platte, eine Platte eines RAID-Verbunds oder ein Dateisystem enthält,
dessen verzeichnete Eigentümer stets gelten (ext2, ext3, ext4, ARXFS),
wird nicht eingehängt, und `mount` nennt den Grund.

## OPTIONS

- `-r, --read-only` — den Datenträger schreibgeschützt einhängen (wie
  `-o ro`).
- `-w, --rw, --read-write` — den Datenträger beschreibbar oder gar
  nicht einhängen: Ein Abbild, in das Sie nicht schreiben dürfen oder
  dessen Dateisystem das Schreiben nicht unterstützt, wird nicht
  eingehängt. Ein späteres `-r` nimmt dies zurück; ein späteres `-o ro`
  oder `-o rw` tut dies nicht. Von `-r`, `-w`, `-o ro` und `-o rw`
  entscheidet die letzte Angabe, ob er schreibgeschützt ist.
- `-t, --types <list>` — die Dateisysteme, kommagetrennt, von denen das
  Abbild eines enthalten muss, benannt wie in der Einhängeliste
  (`iso9660`, `udf`, `adfs`, `affs`, `fat32`), oder `auto` für das, was
  es enthält; eine Liste, die mit `no` beginnt, nennt diejenigen, die es
  nicht enthalten darf. Was das Abbild enthält, wird an seinem Inhalt
  erkannt, daher wählt die Reihenfolge nichts aus: Eine Disc, die sowohl
  UDF als auch ISO 9660 enthält, wird als UDF eingehängt, es sei denn,
  ihr ISO-9660-Dateisystem enthält Rock-Ridge- oder RISC-OS-Datensätze.
  Enthält das Abbild keines dieser Dateisysteme oder nennt die Liste
  keinen Typ, den ein Datenträgerabbild enthält, wird es nicht
  eingehängt.
- `-o, --options <list>` — eine kommagetrennte Liste aus `ro`, `rw`
  (beschreibbar, sofern das möglich ist; die Voreinstellung), `noexec`,
  `loop` und `name=name`, dem Namen, den der Datenträger erhalten soll.
  Als `noexec` und `loop` wird ein Abbild ohnehin eingehängt.
- `-v, --verbose` — melden, wo der Datenträger eingehängt wurde.
- `-l, --show-labels` — für die Auflistung angenommen, die keine
  Bezeichnungen hinzuzufügen hat.
- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `mount` — die eingehängten Dateisysteme auflisten.
- `mount Workbench.adf` — ein Amiga-Diskettenabbild unter
  `/Storage/workbench` einhängen, sofern das seine
  Datenträgerbezeichnung ist.
- `mount -r disc.iso /Storage/disc` — ein Disc-Abbild schreibgeschützt
  als `disc` einhängen.
- `mount -v -t adfs Elite.adl` — ein Acorn-Diskettenabbild einhängen
  und melden, wo.

## EXIT STATUS

- `0` — die Auflistung wurde ausgegeben oder der Datenträger
  eingehängt (oder die Kurzhilfe wurde ausgegeben).
- `1` — die Befehlszeile wurde nicht verstanden.
- `2` — die Einhängeliste konnte nicht gelesen werden, oder die
  Ausgabe auf das Terminal schlug fehl.
- `32` — das Abbild wurde nicht eingehängt.

## ENVIRONMENT

- `LANG` — das bevorzugte Gebietsschema für die Kurzhilfe (ein
  BCP-47-Kennzeichen wie `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
