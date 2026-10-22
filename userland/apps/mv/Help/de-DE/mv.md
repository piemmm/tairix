## NAME

mv — Dateien und Verzeichnisse verschieben (umbenennen)

## SYNOPSIS

`mv [-finvT] [-t dir] [--] source... dest`

## DESCRIPTION

Verschiebt jeden Quelloperanden zu einem Ziel. Bei einer einzigen
Quelle und einem Ziel, das kein Verzeichnis benennt, wird die Quelle
auf genau diesen Pfad umbenannt. Benennt das Ziel ein bestehendes
Verzeichnis — und immer bei mehr als einer Quelle — wird jede Quelle
unter ihrem eigenen Basisnamen *in* dieses Verzeichnis verschoben.

Eine Verschiebung innerhalb eines Datenträgers ist ein atomares
Umbenennen, das die Identität des Knotens bewahrt. Eine Verschiebung
über Datenträger hinweg hält dasselbe Versprechen: Die Kopie wird neben
dem Ziel aufgebaut, auf den Datenträger geschrieben und in einem Schritt
an ihren Platz umbenannt, und erst dann wird die Quelle entfernt — so
enthält das Ziel, was es enthielt, bis die Verschiebung vollständig ist,
und weder ein Fehler noch ein Absturz verliert etwas. In beiden Fällen
ersetzt ein Verzeichnis nur ein leeres Verzeichnis, nichts anderes
ersetzt ein Verzeichnis, und kein Verzeichnis wird in seinen eigenen
Teilbaum verschoben.

Ein bestehendes Ziel wird standardmäßig überschrieben, mit `-n`
übersprungen und mit `-i` über die Standardfehlerausgabe erfragt
(eine abgelehnte Frage überspringt dieses Verschieben ohne Fehler;
eine unlesbare Antwort gilt nie als Zustimmung). Der erste Fehlschlag
beendet den Lauf vor jedem späteren Operanden. `--` beendet die
Optionsauswertung: jedes spätere Argument ist ein Pfad.

Die Kopie übernimmt die Löcher jeder Datei, ihre erweiterten Attribute —
einen RISC OS-Dateityp, einen Mac-Typ und -Ersteller —, ihre
Zugriffsrechte und ihren Eigentümer, wo immer das Ziel sie aufbewahrt,
und verschiebt eine symbolische Verknüpfung als Verknüpfung mit
demselben Ziel, nie das, worauf sie zeigt. Eine Datei, die der
Verschiebende ihrem Eigentümer nicht zurückgeben darf, gehört danach
ihm, ohne ihre Set-ID-Bits. Zeitstempel werden nicht übernommen: Kein
Aufruf dieses Systems setzt sie. Attribute, die das Ziel nicht
aufbewahren kann, werden am Ende des Laufs in einer Warnung gemeldet und
in einem `omission`-Eintrag im Standard-Informationsstrom, bevor die
Quellen endgültig verschwunden sind.

## OPTIONS

- `-f, --force` — vor dem Überschreiben nie nachfragen. Von `-f`, `-i`
  und `-n` gilt das zuletzt angegebene.
- `-i, --interactive` — vor dem Überschreiben eines bestehenden Ziels
  fragen; nur eine mit `y`/`Y` beginnende Antwort stimmt zu.
- `-n, --no-clobber` — ein bestehendes Ziel nie überschreiben.
- `-v, --verbose` — jede Verschiebung als `renamed 'source' -> 'dest'`
  melden, eine über Datenträger hinweg als `copied 'source' -> 'dest'`
  und danach `removed 'source'`.
- `-t dir, --target-directory=dir` — jede Quelle nach `dir`
  verschieben, das ein bestehendes Verzeichnis sein muss. Der Wert
  folgt angehängt (`-tdir`, `--target-directory=dir`) oder als
  nächstes Argument.
- `-T, --no-target-directory` — das Ziel als gewöhnliche Datei
  behandeln; genau eine Quelle ist erlaubt. Nicht mit `-t`
  kombinierbar.
- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `mv draft.txt final.txt` — eine Datei umbenennen.
- `mv -v a.txt b.txt Archive` — beide Dateien nach `Archive`
  verschieben und jedes Verschieben melden.
- `mv -n new.cfg current.cfg` — eine Datei nur installieren, wenn das
  Ziel noch nicht existiert.

## EXIT STATUS

- `0` — jedes Verschieben ist gelungen (ein `-n`-Überspringen und
  eine abgelehnte `-i`-Frage sind keine Fehlschläge).
- `1` — ein Dateisystem-, Frage- oder Ausgabefehler; der Grund wird
  auf der Standardfehlerausgabe gemeldet.
- `2` — die Befehlszeile wurde nicht verstanden.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag
  wie `de-DE`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
