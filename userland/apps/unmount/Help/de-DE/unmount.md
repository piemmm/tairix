## NAME

unmount — einen eingehängten Datenträger aushängen

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Nimmt den unter `name` eingehängten Datenträger außer Betrieb: das
Dateisystem und das Gerät werden geleert, die Einhängung unter
`/Storage` wird entfernt und die dauerhafte `id::`-Wurzel des
Datenträgers wird zurückgezogen. `name` ist der Katalogname des
Datenträgers (`usb1`) oder sein Einhängepfad (`/Storage/usb1`),
abgeglichen mit der Einhängeliste der Systeminformations-API.

Ein Datenträger, dessen Gerät mit noch nicht festgeschriebenen
Schreibvorgängen entfernt wurde, bleibt als `unavailable-dirty`
(oder `unavailable-lost`) in der Einhängeliste sichtbar, und ein
einfaches `unmount` verweigert: die zurückgehaltenen Daten werden für
ein geprüftes Wiedereinstecken aufbewahrt. Auch bei einem Datenträger,
auf dem noch Dateien geöffnet sind oder auf dem ein Programm sein
Arbeitsverzeichnis hat, verweigert ein einfaches `unmount`. `--force`
ist der bewusste Ausweg — noch geöffnete Dateien auf dem Datenträger
werden abgetrennt, ein Programm, das darauf arbeitet, bleibt in einem
Verzeichnis zurück, das es nicht mehr gibt, die zurückgehaltenen Daten
werden verworfen, der Datenträger wird entfernt, und was aufgegeben
wurde, wird im Prüfprotokoll vermerkt. Bei einem gesunden Datenträger
leert und trennt `--force` weiterhin sauber; nichts wird verworfen,
wenn ein sauberes Festschreiben möglich ist.

Das Aushängen erfordert die Einhängeberechtigung (`CAP_FS_MOUNT`);
der Kernel prüft sie und protokolliert jede Entscheidung. Die
permanenten Startdatenträger und die Sichtbindungen des Systems
lassen sich nicht aushängen.

## OPTIONS

- `-f, --force` — erzwungenes Aushängen: den Datenträger auch dann
  entfernen, wenn Dateien auf ihm geöffnet sind, ein Programm darauf
  arbeitet oder sich seine noch nicht festgeschriebenen Daten nicht
  festschreiben lassen; die geöffneten Dateien werden abgetrennt und die
  zurückgehaltenen Daten verworfen.
- `-?, --help` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `unmount usb1` — den als `usb1` eingehängten Datenträger sauber
  aushängen.
- `unmount /Storage/usb1` — dasselbe, über den Einhängepfad benannt.
- `unmount --force usb1` — einen nicht verfügbaren Datenträger
  entfernen und seine zurückgehaltenen, nicht festgeschriebenen Daten
  verwerfen.

## EXIT STATUS

- `0` — der Datenträger wurde ausgehängt (oder die Kurzhilfe wurde
  ausgegeben).
- `1` — der Datenträger wurde nicht gefunden, ist nicht aushängbar
  oder der Kernel hat das Aushängen verweigert. Eine nicht verstandene
  Befehlszeile endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — das bevorzugte Gebietsschema für die Kurzhilfe (ein
  BCP-47-Kennzeichen wie `de-DE`).

## SEE ALSO

- `mount`
- `df`
- `man`
