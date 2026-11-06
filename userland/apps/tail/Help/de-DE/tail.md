## NAME

tail — den letzten Teil von Dateien ausgeben

## SYNOPSIS

`tail [option...] [file...]`

## DESCRIPTION

Gibt die letzten 10 Zeilen jeder `file` auf der Standardausgabe aus. Bei
mehreren `file` wird jedem Teil eine Kopfzeile `==> file <==`
vorangestellt. Ohne `file`, oder wenn `file` gleich `-` ist, wird die
Standardeingabe gelesen.

`-n` und `-c` ändern die ausgegebene Menge: eine einfache Anzahl (oder
eine mit führendem `-`) gibt die letzten `num` Zeilen oder Bytes aus;
eine Anzahl mit führendem `+` gibt alles **ab** Zeile oder Byte `num`
(gezählt ab 1) bis zum Ende aus. Eine Anzahl kann ein
Multiplikatorsuffix tragen: `b` (512), `kB` (1000), `K` (1024), `MB`,
`M`, `GB`, `G` und so weiter für `T`, `P`, `E`, `Z`, `Y`, `R`, `Q` (ein
einzelner Buchstabe multipliziert mit Potenzen von 1024; mit `B` mit
Potenzen von 1000; mit `iB` mit Potenzen von 1024).

Die historische Erst-Argument-Form `tail -num` / `tail +num` (mit einem
optionalen abschließenden Buchstaben `b`/`c`/`l`) wird akzeptiert, wie im
GNU-Werkzeug.

Der Folgemodus hält jede Datei geöffnet und gibt neue Daten aus, sobald
die Datei wächst. `-f` folgt der geöffneten Datei über den Deskriptor —
nach einem Umbenennen oder Verschieben folgt es weiter derselben Datei.
`-F` folgt dem *Namen*: wird die Datei rotiert (wegbenannt und durch
eine neue Datei an ihrer Stelle ersetzt), öffnet es die neue Datei, und
`--retry` versucht es weiter mit einem Namen, den es noch nicht gibt.
Ein Folgen blockiert, bis sich die Datei tatsächlich ändert — es lässt
die CPU nie leer drehen. `--pid=PID` beendet das Folgen, sobald Prozess
`PID` endet, geprüft alle `--sleep-interval` (Sekunden, Standard 1).
`--max-unchanged-stats=N` legt fest, wie viele ruhige Zyklen vergehen,
bevor `-F` den Namen erneut auf eine Rotation prüft (Standard 5). Eine
Kürzung (die Datei schrumpft) wird gemeldet, und der Datei wird ab ihrem
neuen Anfang erneut gefolgt.

Wenn führender Inhalt nicht angezeigt wird, wird ein Hinweisdatensatz
auf den Standard-Informationsstrom (fd 3) geschrieben; er ändert nie die
Ausgabe oder den Exit-Status. Eine nicht lesbare Datei wird auf der
Standardfehlerausgabe gemeldet und der Lauf fährt mit der nächsten Datei
fort.

An einem Terminal wird jeder ausgegebene Name dort in Anführungszeichen
gesetzt, wo eine Shell sie bräuchte, und jedes Steuer- oder sonst
unsichtbare Zeichen darin maskiert geschrieben, sodass ein Name den
Bildschirm weder steuern noch umordnen kann; GNU gibt ihn unverändert
aus. Eine Pipe oder eine Datei erhält den Namen Byte für Byte.

## OPTIONS

- `-c, --bytes <num>` — die letzten `num` Bytes jeder Datei ausgeben;
  mit führendem `+` alles ab Byte `num`.
- `-n, --lines <num>` — die letzten `num` Zeilen jeder Datei ausgeben;
  mit führendem `+` alles ab Zeile `num`.
- `-q, --quiet, --silent` — die Kopfzeilen `==> file <==` nie ausgeben.
- `-v, --verbose` — die Kopfzeilen `==> file <==` immer ausgeben.
- `-z, --zero-terminated` — Zeilen sind NUL-getrennt statt
  zeilenumbruchgetrennt.
- `-f, --follow[=descriptor]` — die Datei geöffnet halten und angehängte
  Daten ausgeben, wobei ihr über den Deskriptor gefolgt wird.
- `-F` — dem Namen folgen (`--follow=name --retry`): die Datei erneut
  öffnen, wenn sie rotiert wird.
- `--follow=name` — dem Namen statt dem Deskriptor folgen.
- `--retry` — weiter versuchen, eine noch nicht zugängliche Datei zu
  öffnen.
- `--pid <PID>` — beim Folgen aufhören, sobald Prozess `PID` endet.
- `--sleep-interval <N>` — Sekunden zwischen den erneuten Prüfungen von
  `--pid`/Rotation (Bruchteile erlaubt; Standard 1).
- `--max-unchanged-stats <N>` — ruhige Zyklen, bevor `-F` den Namen
  erneut auf eine Rotation prüft (Standard 5).
- `-h, -?` — die eigene Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `tail log.txt` — die letzten 10 Zeilen von `log.txt` ausgeben.
- `tail -n 3 a b` — die letzten 3 Zeilen von `a` und `b` ausgeben, je
  unter ihrer Kopfzeile.
- `tail -c 1K image` — die letzten 1024 Bytes von `image` ausgeben.
- `tail -n +5 notes` — `notes` ab der 5. Zeile ausgeben.
- `tail -f log.txt` — die letzten 10 Zeilen ausgeben, danach neue Zeilen,
  sobald sie geschrieben werden.
- `tail -F /System/Logs/system` — dem Systemprotokoll über Rotationen
  hinweg folgen.
- `tail -f --pid 1234 out` — `out` folgen, bis Prozess 1234 endet.

## EXIT STATUS

- `0` — jede Datei wurde ausgegeben (oder die Kurzhilfe wurde
  geschrieben).
- `1` — eine Datei konnte nicht gelesen werden, oder die Ausgabe konnte
  nicht geliefert werden. Eine nicht verstandene Befehlszeile endet
  ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag wie
  `fr-FR`).

## SEE ALSO

- `head`
- `cat`
- `wc`
- `man`
