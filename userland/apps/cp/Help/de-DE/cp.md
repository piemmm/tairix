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
abgelehnte Frage überspringt diese Kopie und lässt den Lauf
fehlschlagen, wie bei GNU; eine unlesbare Antwort gilt nie als
Zustimmung).
Ein Ziel, das die Datei der Quelle selbst ist — derselbe Name, ein
weiterer Name für sie oder ein Link auf sie —, wird als dieselbe Datei
abgelehnt und bleibt, wie es war, außer dass `-l` es als bereits
verknüpft ansieht. Eine Verknüpfung, die `-f` an die Stelle eines
bestehenden Namens setzt, nimmt diese Stelle in einem Schritt ein und
wird angelegt, bevor etwas ersetzt wird, sodass eine, die sich nicht
anlegen lässt, den Namen bestehen lässt.

Eine Datei, die sich nicht lesen, anlegen oder schreiben lässt, wird
gemeldet, und die übrigen werden trotzdem kopiert, auch der Rest ihres
Verzeichnisses. `--` beendet die Optionsauswertung: jedes spätere
Argument ist ein Pfad.

Eine neue Datei erhält die Berechtigungsbits ihrer Quelle ohne die
set-ID- und Sticky-Bits, abzüglich der umask, sodass die Kopie einer
privaten Datei privat bleibt; ein bereits bestehendes Ziel behält seinen
eigenen Modus. Ein neues Verzeichnis erhält am Ende die Berechtigungs-
und Sticky-Bits seiner Quelle abzüglich der umask; solange seine
Einträge hineinkopiert werden, steht es seinem Eigentümer offen, sodass
ein schreibgeschütztes Quellverzeichnis trotzdem vollständig kopiert
wird, ebenso unter einer umask, die dem Eigentümer das Schreibrecht
vorenthält. Eine innerhalb eines Zielbaums angetroffene symbolische
Verknüpfung wird beim Schreiben nie verfolgt.

GNUs `-a`/`--archive` und `--preserve` mit einer anderen Menge als
`--preserve=links,xattr` werden **abgelehnt**, nicht eingeschränkt. `-a`
ist `-dR --preserve=all`, und `--preserve=all` umfasst die Zeitstempel
eines Knotens, die kein Aufruf dieses Systems setzen kann — `-a` zu
befolgen würde also eine Bewahrung melden, die nicht stattgefunden hat.
Den Rest davon deckt `-dR` ab.

Jede Kopie überträgt die erweiterten Attribute der Quelle — einen
RISC-OS-Dateityp, Typ und Ersteller eines Mac — überall dort, wo das
Ziel sie speichern kann, und bewahrt die Lücken einer dünn besetzten
Datei als Lücken. Attribute, die ein Ziel nicht speichern kann, werden
am Ende des Laufs in einer einzigen Warnung gemeldet und in einem
`omission`-Datensatz auf dem Standard-Informationsstrom; mit
`--preserve=xattr` schlägt eine solche Kopie stattdessen fehl.

Ein Verzeichnis wird nie in sich selbst kopiert: Ein Ziel innerhalb der
Quelle — wie auch immer es geschrieben ist, über eine Verknüpfung, ein
`..` oder einen anderen Namen des Datenträgers — wird abgelehnt, bevor
irgendetwas geschrieben ist, und ein Verzeichnis, das während der Kopie
in die Quelle verschoben wird, beendet die Kopie dort, wo der Durchlauf
es antrifft.

## OPTIONS

- `-r, -R, --recursive` — Verzeichnisse mitsamt Inhalt kopieren.
- `-f, --force` — wenn eine Zieldatei nicht angelegt werden kann, sie
  entfernen und die Kopie einmal wiederholen; mit `-l`, `-s` oder bei
  einer Verknüpfung, die `-P` nachbildet, die Verknüpfung an die Stelle
  eines bestehenden Namens setzen.
- `-i, --interactive` — vor dem Überschreiben einer bestehenden Datei
  fragen; nur eine mit `y`/`Y` beginnende Antwort stimmt zu, und eine
  abgelehnte Frage überspringt diese Datei und lässt den Lauf
  fehlschlagen.
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
  eine Kopie, deren Ziel eines davon nicht speichern kann, schlägt fehl
  und nennt es. `--preserve=links,xattr` verlangt beides.
- `--no-preserve=xattr` — keine erweiterten Attribute kopieren.
  `ownership` und `timestamps` werden ebenfalls angenommen und ohnehin
  nicht bewahrt.
- `--no-preserve=mode` — jede neue Datei und jedes neue Verzeichnis mit
  dem Modus anlegen, den ein neues erhält, wenn nichts einen nennt,
  abzüglich der umask, statt mit dem ihrer Quelle. `--no-preserve=all`
  tut dies ebenfalls.
- `--sparse=WHEN` — wie eine dünn besetzte Datei kopiert wird: `auto`,
  die Voreinstellung, bewahrt ihre Lücken; `always` lässt zusätzlich
  jeden gespeicherten Block aus Nullen ungeschrieben; `never` schreibt
  jedes Byte.
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

- `0` — jede Kopie ist gelungen (ein `-n`-Überspringen ist kein
  Fehlschlag).
- `1` — eine Datei ließ sich nicht kopieren, eine `-i`-Frage wurde
  abgelehnt, oder das Fragen oder eine Ausgabe schlug fehl; der Grund
  jedes Fehlschlags wird auf der Standardfehlerausgabe gemeldet. Eine
  nicht verstandene Befehlszeile endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag
  wie `de-DE`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
