## NAME

clear — den Terminalbildschirm löschen

## SYNOPSIS

`clear [-x]`

## DESCRIPTION

Schreibt die Sequenz, die den Cursor in die linke obere Ecke setzt und
die gesamte Anzeige löscht, sodass ein leerer Bildschirm bleibt. Welche
Sequenz geschrieben wird, bestimmt das in `TERM` benannte Terminal; ein
Terminal, das nicht löschen kann (ein unbekanntes `TERM` fällt auf das
Minimalprofil zurück), lässt den Befehl fehlschlagen, statt Bytes zu
drucken, die das Terminal als Zeichenmüll darstellen würde.

Sofern das Terminal es unterstützt, werden auch die Zeilen gelöscht,
die es oberhalb des Bildschirms aufbewahrt — der Verlaufspuffer —,
sodass nichts, was auf dem Bildschirm stand, durch Zurückscrollen
wieder sichtbar wird.

## OPTIONS

- `-x` — den Verlaufspuffer behalten: nur den Bildschirm löschen.
- `-h, -?` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `clear` — den Bildschirm löschen.

## EXIT STATUS

- `0` — die Löschsequenz wurde geschrieben.
- `1` — das Terminal kann nicht löschen, oder die Ausgabe konnte nicht
  zugestellt werden. Eine nicht verstandene Befehlszeile endet ebenfalls
  mit `1`.

## ENVIRONMENT

- `TERM` — das Terminal, dessen Löschsequenz geschrieben wird.
- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Kürzel
  wie `de-DE`).

## SEE ALSO

- `reset`
- `man`
