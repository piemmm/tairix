## NAME

unmount — einen eingehängten Datenträger aushängen

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Nimmt den unter `name` eingehängten Datenträger außer Betrieb: das
Dateisystem und das Gerät werden geleert, die Einhängung unter
`/Storage` wird entfernt und die dauerhafte `id::`-Wurzel des
Datenträgers wird zurückgezogen. `name` ist der Katalogname des
Datenträgers (`usb1`), ein Verzeichnis, in das er eingehängt ist
(`/Storage/usb1` oder das, an das `mount abbild verzeichnis` ihn
gebunden hat), oder der Dateiname des Datenträgerabbilds, aus dem er
eingehängt wurde (`Workbench.adf`), abgeglichen mit der Einhänge- und
der Sichtliste der Systeminformations-API; ein Dateiname, den auch das
Abbild eines anderen Benutzers trägt, bezeichnet Ihr eigenes. Ein
Dateiname, der mehr als einen Datenträger meinen könnte, oder einer,
von dem die Liste nur den Anfang behält, bezeichnet keinen: nennen Sie
den Datenträger dann dort, wo er eingehängt ist.
Ein Pfad darf relativ sein oder auf einen Schrägstrich enden: er wird
aufgelöst, wie `mount` einen auflöst. Der private Datenträger eines
anderen Benutzers nennt weder seinen Dateinamen noch die Verzeichnisse,
an die er gebunden ist; er wird daher dort genannt, wo er unter
`/Storage` eingehängt ist.

Ein Datenträger, dessen Gerät mit noch nicht festgeschriebenen
Schreibvorgängen entfernt wurde, bleibt als `unavailable-dirty` in der
Einhängeliste sichtbar, und ein einfaches `unmount` verweigert: die
zurückgehaltenen Daten werden für ein geprüftes Wiedereinstecken
aufbewahrt. Ebenso verweigert es bei einem wieder eingesteckten
Datenträger, bei dem sich nicht nachweisen ließ, dass er unverändert
ist (`recovery-conflict`), und der diese Daten weiterhin aufbewahrt,
sowie bei einem, dessen noch nicht festgeschriebene Schreibvorgänge
nicht erhalten werden konnten (`unavailable-lost`), damit ihr Verlust
anerkannt statt übergangen wird. Auch bei einem Datenträger, auf dem
noch Dateien geöffnet sind oder auf dem ein Programm sein
Arbeitsverzeichnis hat, verweigert ein einfaches `unmount`. `--force`
ist der bewusste Ausweg — noch geöffnete Dateien auf dem Datenträger
werden abgetrennt, ein Programm, das darauf arbeitet, bleibt in einem
Verzeichnis zurück, das es nicht mehr gibt, die zurückgehaltenen Daten
werden verworfen, der Datenträger wird entfernt, und was aufgegeben
wurde, wird im Prüfprotokoll vermerkt. Bei einem gesunden Datenträger
leert und trennt `--force` weiterhin sauber; nichts wird verworfen,
wenn ein sauberes Festschreiben möglich ist.

Ein Datenträgerabbild oder eine Netzwerkfreigabe, die Sie selbst
eingehängt haben, dürfen Sie aushängen; das Aushängen jedes anderen
Datenträgers erfordert die Einhängeberechtigung (`CAP_FS_MOUNT`).
Der Kernel prüft sie und protokolliert jede Entscheidung. Die
permanenten Startdatenträger und die Sichtbindungen des Systems
lassen sich nicht aushängen.

## OPTIONS

- `-f, --force` — erzwungenes Aushängen: den Datenträger auch dann
  entfernen, wenn Dateien auf ihm geöffnet sind, ein Programm darauf
  arbeitet oder sich seine noch nicht festgeschriebenen Daten nicht
  festschreiben lassen; die geöffneten Dateien werden abgetrennt und die
  zurückgehaltenen Daten verworfen.
- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `unmount usb1` — den als `usb1` eingehängten Datenträger sauber
  aushängen.
- `unmount /Storage/usb1` — dasselbe, über den Einhängepfad benannt.
- `unmount Workbench.adf` — das aus `Workbench.adf` eingehängte
  Datenträgerabbild aushängen.
- `unmount --force usb1` — einen nicht verfügbaren Datenträger
  entfernen und seine zurückgehaltenen, nicht festgeschriebenen Daten
  verwerfen.

## EXIT STATUS

- `0` — der Datenträger wurde ausgehängt (oder die Kurzhilfe wurde
  ausgegeben).
- `1` — die Befehlszeile wurde nicht verstanden, oder der Aufrufer darf
  den Datenträger nicht aushängen.
- `2` — der Systeminformationsdienst oder das Schreiben auf das
  Terminal ist fehlgeschlagen.
- `32` — der Datenträger war nicht eingehängt oder ist nicht aushängbar,
  der ihn nennende Pfad ließ sich nicht auflösen, oder das Aushängen ist
  fehlgeschlagen.

## ENVIRONMENT

- `LANG` — das bevorzugte Gebietsschema für die Kurzhilfe (ein
  BCP-47-Kennzeichen wie `de-DE`).

## SEE ALSO

- `mount`
- `df`
- `man`
