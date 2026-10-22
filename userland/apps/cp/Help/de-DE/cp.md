## NAME

cp — Dateien und Verzeichnisse kopieren

## SYNOPSIS

`cp [-dfHilLnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

## DESCRIPTION

Kopiert jeden Quelloperanden zu einem Ziel. Bei einer einzigen Quelle
und einem Ziel, das kein Verzeichnis benennt, wird die Quelle auf
genau diesen Pfad kopiert. Benennt das Ziel ein bestehendes
Verzeichnis — und immer bei mehr als einer Quelle — wird jede Quelle
unter ihrem eigenen Basisnamen *in* dieses Verzeichnis kopiert.

Eine Verzeichnisquelle wird nur mit `-r` kopiert, das den ganzen
Teilbaum nachbildet; ohne `-r` wird ein Verzeichnisoperand abgelehnt.
Eine bestehende Zieldatei wird standardmäßig überschrieben, mit `-n`
übersprungen und mit `-i` über die Standardfehlerausgabe erfragt (eine
abgelehnte Frage überspringt diese Kopie ohne Fehler; eine unlesbare
Antwort gilt nie als Zustimmung).
Ein Ziel, das die Datei der Quelle selbst ist — derselbe Name, ein
weiterer Name für sie oder ein Link auf sie —, wird als dieselbe Datei
abgelehnt und bleibt, wie es war.

Der erste Fehlschlag beendet den Lauf vor jedem späteren Operanden.
`--` beendet die Optionsauswertung: jedes spätere Argument ist ein
Pfad.

Jede Kopie überträgt die erweiterten Attribute der Quelle — einen
RISC-OS-Dateityp, Typ und Ersteller eines Mac — überall dort, wo das Ziel
sie speichern kann, und bewahrt die Lücken einer dünn besetzten Datei als
Lücken. Attribute, die ein Ziel nicht speichern kann, werden am Ende des
Laufs in einer einzigen Warnung gemeldet und in einem `omission`-Datensatz
auf dem Standard-Informationsstrom; mit `--preserve=xattr` schlägt eine
solche Kopie stattdessen fehl.

Ein Verzeichnis wird nie in sich selbst kopiert: Ein Ziel innerhalb der
Quelle — wie auch immer es geschrieben ist, über eine Verknüpfung, ein
`..` oder einen anderen Namen des Datenträgers — wird abgelehnt, bevor
irgendetwas geschrieben ist, und ein Verzeichnis, das während der Kopie
in die Quelle verschoben wird, beendet die Kopie dort, wo der Durchlauf
es antrifft.

## OPTIONS

- `-r, -R, --recursive` — Verzeichnisse mitsamt Inhalt kopieren.
- `-f, --force` — wenn eine Zieldatei nicht angelegt werden kann, sie
  entfernen und die Kopie einmal wiederholen.
- `-i, --interactive` — vor dem Überschreiben einer bestehenden Datei
  fragen; nur eine mit `y`/`Y` beginnende Antwort stimmt zu.
- `-n, --no-clobber` — eine bestehende Datei nie überschreiben. Das
  spätere von `-i` und `-n` gewinnt.
- `-l, --link` — dem Ziel einen zweiten Namen für den Knoten der Quelle
  geben statt deren Bytes zu kopieren, sodass die beiden Namen bei einem
  späteren Schreibvorgang nicht auseinandergehen können. Eine
  Quellverzeichnis braucht weiterhin `-r`.
- `-s, --symbolic-link` — eine symbolische Verknüpfung anlegen, die die
  Quelle benennt, statt sie zu kopieren.
- `-P, --no-dereference` — jede symbolische Verknüpfung unter den
  Quellen als Verknüpfung mit demselben Ziel nachbilden, wörtlich, statt
  zu kopieren, worauf sie zeigt (so übersteht eine relative oder ins
  Leere zeigende Verknüpfung die Kopie).
- `-L, --dereference` — jeder symbolischen Verknüpfung unter den Quellen
  folgen und kopieren, worauf sie zeigt.
- `-H` — einer als Operand genannten symbolischen Verknüpfung folgen,
  die innerhalb eines Baums angetroffenen aber nachbilden. Ohne `-H`,
  `-L` oder `-P` bildet eine rekursive Kopie jede Verknüpfung nach, auch
  die ihrer Operanden, und jede andere Kopie folgt den Verknüpfungen
  ihrer Operanden, wie bei GNU; `-l` verknüpft, worauf eine verfolgte
  Verknüpfung zeigt.
- `--preserve=links` — zwei Quellen, die einen Knoten benennen, erhalten
  am Ziel zwei Namen statt zweier Kopien, sodass die Kopie den Speicher
  nicht stillschweigend verdoppelt.
- `--preserve=xattr` — jedes erweiterte Attribut muss übertragen werden:
  eine Kopie, deren Ziel eines davon nicht speichern kann, schlägt fehl und
  nennt es. `--preserve=links,xattr` verlangt beides.
- `--no-preserve=xattr` — keine erweiterten Attribute kopieren. `mode`,
  `ownership` und `timestamps` werden ebenfalls angenommen und ohnehin nicht
  bewahrt.
- `--sparse=WHEN` — wie eine dünn besetzte Datei kopiert wird: `auto`, die
  Voreinstellung, bewahrt ihre Lücken; `always` lässt zusätzlich jeden
  gespeicherten Block aus Nullen ungeschrieben; `never` schreibt jedes Byte.
- `-d` — `-P` und `--preserve=links` zusammen, wie im GNU-Werkzeug.
- `-v, --verbose` — jede Kopie als `'source' -> 'dest'` melden.
- `-t dir, --target-directory=dir` — jede Quelle nach `dir` kopieren,
  das ein bestehendes Verzeichnis sein muss. Der Wert folgt angehängt
  (`-tdir`, `--target-directory=dir`) oder als nächstes Argument.
- `-T, --no-target-directory` — das Ziel als gewöhnliche Datei
  behandeln; genau eine Quelle ist erlaubt. Nicht mit `-t`
  kombinierbar.
- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `cp notes.txt backup.txt` — eine Datei unter neuem Namen kopieren.
- `cp -r Projects Archive` — den Baum `Projects` in `Archive`
  nachbilden (oder als `Archive`, wenn es nicht existiert).
- `cp -v -t Backup a.txt b.txt` — beide Dateien nach `Backup`
  kopieren und jede Kopie melden.

## EXIT STATUS

- `0` — jede Kopie ist gelungen (ein `-n`-Überspringen und eine
  abgelehnte `-i`-Frage sind keine Fehlschläge).
- `1` — ein Dateisystem-, Frage- oder Ausgabefehler; der Grund wird
  auf der Standardfehlerausgabe gemeldet.
- `2` — die Befehlszeile wurde nicht verstanden.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag
  wie `de-DE`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
