## NAME

label — den Titel eines Datenträgers anzeigen oder ändern

## SYNOPSIS

`label [option...] name [title]`

## DESCRIPTION

Zeigt den Titel des Datenträgers, den `name` bezeichnet, oder setzt ihn,
wenn `title` angegeben ist. Der Titel eines Datenträgers ist die
Bezeichnung, die sein Dateisystem speichert: der Name, unter dem er
angezeigt wird, und der Name, den er unter `/Storage` beim nächsten
Einbinden erhält. `name` ist der Katalogname des Datenträgers (`usb1`),
ein Verzeichnis, in dem er eingebunden ist (`/Storage/usb1`), der
Dateiname des Speicherabbilds, aus dem er eingebunden wurde
(`Workbench.adf`), oder seine dauerhafte Wurzel (`id::…`), benannt wie
bei `unmount`. Ein leerer `title` löscht den Titel, wo das Format des
Datenträgers es erlaubt.

Solange der Datenträger eingebunden bleibt, ändert sich sein Name unter
`/Storage` nicht, sodass nichts, was ihn dort benennt, aufhört zu
funktionieren; der neue Titel benennt ihn ab dem nächsten Einbinden.
Seine Identität und die seiner Dateien bleiben, wie sie waren.

Jedes Format schreibt einen Titel auf seine Weise, und ein Titel, den es
nicht speichern kann, wird abgelehnt statt gekürzt. Ein FAT-Datenträger
nimmt bis zu elf Großbuchstaben, Ziffern, Leerzeichen und die
Satzzeichen, die ein kurzer Dateiname erlaubt, nie mit einem Leerzeichen
am Anfang oder Ende; ein ext2-, ext3- oder ext4-Datenträger bis zu
sechzehn Byte; ein ADFS-Datenträger bis zu zehn Zeichen; ein
AmigaDOS-Datenträger eins bis dreißig; ein UDF-Datenträger bis zu 126;
und ein ARXFS-Datenträger bis zu 252 Byte, sofern er nicht verschlüsselt
ist. Eine mit Titel gemasterte Disc, ein Archiv und eine
Netzwerkfreigabe nehmen keinen neuen an.

Ein Speicherabbild, das Sie selbst eingebunden haben, dürfen Sie
umbenennen. Einen Wechseldatenträger — einen USB-Stick, eine Karte —
dürfen Sie umbenennen, während Sie den Computer selbst benutzen, sofern
Sie den Inhalt seines obersten Ordners ändern dürfen; jeder andere
Datenträger erfordert die Einbindebefugnis (`CAP_FS_MOUNT`), die ein
Administrator hat. Der Kernel prüft dies und hält jede Entscheidung im
Prüfprotokoll fest.

## OPTIONS

- `-h, -?, --help` — die eigene Kurzhilfe dieses Befehls anzeigen.
- `--version` — Namen und Version dieses Befehls anzeigen.

## EXAMPLES

- `label usb1` — den Titel des als `usb1` eingebundenen Datenträgers
  anzeigen.
- `label usb1 HOLIDAY` — ihm den Titel `HOLIDAY` geben.
- `label /Storage/backup 'Backup 2026'` — dem unter `/Storage/backup`
  eingebundenen Datenträger einen Titel geben.
- `label usb1 ''` — seinen Titel löschen.

## EXIT STATUS

- `0` — der Titel wurde angezeigt oder gesetzt (oder die Hilfe oder die
  Version ausgegeben).
- `1` — die Befehlszeile wurde nicht verstanden, der Name bezeichnete
  keinen Datenträger, der Titel war keiner, oder der Datenträger hat ihn
  abgelehnt.
- `2` — der Systeminformationsdienst oder die Ausgabe auf das Terminal
  ist fehlgeschlagen.

## ENVIRONMENT

- `LANG` — das bevorzugte Gebietsschema für die Kurzhilfe (ein
  BCP-47-Tag wie `fr-FR`).

## SEE ALSO

- `mount`
- `unmount`
- `df`
