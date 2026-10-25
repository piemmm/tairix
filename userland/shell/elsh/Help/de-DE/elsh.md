## NAME

elsh — die TAIRiX-Befehlsshell

## SYNOPSIS

`elsh [-h | -?]`

## DESCRIPTION

Startet eine interaktive Befehlsshell — eine
Lese-Auswerte-Ausgabe-Schleife über die geerbten Standardströme. Ein
getipptes Befehlswort wird zuerst gegen die eingebauten Befehle der
Shell aufgelöst, dann im System-Befehls-Store (`/System/Commands`), im
System-App-Store (`/System/Applications`), im eigenen Befehls-Store
(`<home>/Commands`) und App-Store (`<home>/Applications`) des
Benutzers, dann in den Verzeichnissen der Variablen `PATH`; diese vier
Stores bilden ein festes Präfix, das der Benutzer weder umordnen noch
überschreiben kann, sodass `PATH` nie einen Systembefehl überdecken
kann. Ein nicht aufgelöstes Wort endet mit `127`; ein aufgelöstes, aber
nicht ausführbares Bundle endet mit `126`.

Die eingebauten Befehle:

- `cd [-L | -P [-e]] [directory]`, `cd -` — das Arbeitsverzeichnis
  wechseln: nach `directory`, ohne Angabe nach `HOME` oder mit `-`
  zurück nach `OLDPWD`, das dabei ausgegeben wird. `-L`, die Vorgabe,
  wendet `directory` so, wie es geschrieben ist, auf `PWD` an, solange
  `PWD` noch auf das Arbeitsverzeichnis verweist, sodass `cd link` und
  danach `cd ..` dorthin zurückführt, wo die Verknüpfung liegt; `-P`
  folgt zuerst den Verknüpfungen, und `PWD` wird zum eigenen Pfad des
  Verzeichnisses (`-e` schlägt dann fehl, wenn sich dieser Pfad nicht
  lesen lässt). `/..` ist `/`, und unterhalb einer Alias-Wurzel
  (`Home:/`) steigt ein `..` nie über sie hinaus.
- `pwd [-L | -P]` — das Arbeitsverzeichnis ausgeben: `PWD`, solange es
  noch darauf verweist (`-L`, die Vorgabe), oder den eigenen Pfad des
  Verzeichnisses (`-P`).
- `echo ...` — die eigenen Operanden ausgeben.
- `export NAME=value`, `unset NAME` — die exportierte Umgebung
  bearbeiten.
- `jobs`, `fg`, `bg` — Jobkontrolle.
- `ulimit [-a] [-H | -S] [resource [value]]` — Ressourcenlimits lesen und
  setzen; `ulimit -n [value]` liest oder setzt das Limit für offene Dateien.
- `umask [-S] [mode]` — die Dateimodus-Erstellungsmaske anzeigen
  (`0022`) oder mit `-S` die Rechte, die sie übrig lässt
  (`u=rwx,g=rx,o=rx`), oder sie setzen: ein oktaler `mode` ist die Maske
  selbst, ein symbolischer (`g-w`, `u=rwx,g=rx,o=`) bearbeitet die
  Rechte, die sie übrig lässt. Jeder danach gestartete Befehl legt seine
  Dateien unter ihr an, und eine Umleitung legt ihre Datei als `0666`
  abzüglich der Maske an.
- `elevate <user> <program> [argument ...]` — einen einzelnen Befehl
  mit den angegebenen Argumenten ausführen, neu authentifiziert über
  den Anmelde-Supervisor der Konsole.
- `shopt [-pqsu] [-o] [name ...]` — die Optionen für Dateinamensmuster
  anzeigen oder setzen.
- `help` — die eingebauten Befehle auflisten.
- `exit [code]` — die Sitzung beenden.

Ein Wort mit einem ungequoteten `*`, `?` oder `[...]` ist ein
Dateinamensmuster und wird wie in bash durch die sortierten Namen
ersetzt, auf die es passt: `cp *.tmp /tmp`. Ein führender `.` muss durch
einen `.` getroffen werden, und ein Muster ohne Treffer bleibt
unverändert. Geschweifte Klammern werden zuerst expandiert: `a{b,c}`
ergibt `ab ac`, und `{1..5}` zählt. Quoting macht jedes dieser Zeichen
wörtlich, und der Wert einer Variable wird nie als Muster gelesen.
`shopt -s` schaltet die bash-Optionen ein: `nullglob` (kein Treffer
ergibt kein Wort), `failglob` (kein Treffer ist ein Fehler), `dotglob`,
`nocaseglob`, `globstar` (`**` passt auf jede Tiefe) und `extglob`
(`?(…)`, `*(…)`, `+(…)`, `@(…)`, `!(…)`); `shopt -o -s noglob` schaltet
die Dateinamensmuster ab.

Auf einem Terminal bietet die Shell einen interaktiven Zeileneditor mit
den vertrauten bash/zsh-Tasten: Pfeil-hoch/-runter (oder
`Ctrl-P`/`Ctrl-N`) blättern durch den Befehlsverlauf der Sitzung,
`Ctrl-R` durchsucht ihn inkrementell, Pfeil-links/-rechts, Pos1/Ende
und `Ctrl-A`/`Ctrl-E` bewegen den Cursor,
`Ctrl-K`/`Ctrl-U`/`Ctrl-W`/`Ctrl-Y` schneiden aus und fügen wieder ein,
`Ctrl-T` vertauscht zwei Zeichen, `Ctrl-L` zeichnet auf einem
geleerten Bildschirm neu, `Ctrl-C` verwirft die gerade bearbeitete
Zeile, und `Ctrl-D` auf leerer Zeile beendet die Sitzung. Tab
vervollständigt das Wort unter dem Cursor: Befehlsnamen (eingebaute
Befehle und installierte Befehls-Apps), Dateipfade und — für ein
Umleitungsziel oder ein Wort in Referenzform — Ressourcenreferenzen
wie `sys:random`. Ein Namensraum vervollständigt seine registrierten
Selektoren segmentweise (`state:` → `net/` → `wan/` → `link`); wo die
Registry nur die Form kennt — ein Schnittstellenname, eine
Interruptleitung —, bietet Tab die echten Namen der Maschine an, oder
gar nichts, wenn diese Sitzung sie nicht auflisten darf.
`info:`/`state:`/`stats:` erscheinen als Argumente und nach `<`, das
den Wert liest (`cat < info:mem/physical`), aber nie nach `>`: eine
solche Ressource wird niemals durch Umleitung geschrieben. Eingaben
aus einer Pipe oder einem Skript umgehen den Editor und verhalten sich
mit und ohne Terminal gleich.

Die Shell nimmt keine Operanden an: die Ausführung von Skripten gehört
noch nicht zu ihrer Grammatik.

## OPTIONS

- `-h, -?` — die Kurzhilfe dieses Befehls anzeigen und beenden.

## EXIT STATUS

- Der Code des eingebauten `exit`, oder `0`, wenn der Eingabestrom
  endet (oder die Kurzhilfe angezeigt wurde).
- `2` — der Aufruf wurde nicht verstanden.

## ENVIRONMENT

- `HOME` — wohin `cd` ohne Operand wechselt.
- `PWD`, `OLDPWD` — das Arbeitsverzeichnis, wie `cd` es erreicht hat,
  und das vorherige; `cd` setzt beide. Ein `PWD`, mit dem die Shell
  startet, bleibt erhalten, solange es auf das Arbeitsverzeichnis
  verweist, und wird andernfalls zum eigenen Pfad dieses
  Verzeichnisses.
- `PATH` — die nach dem festen Store-Präfix durchsuchten Verzeichnisse.
- `GLOBIGNORE` — durch `:` getrennte Muster, deren Treffer ein
  Dateinamensmuster verwirft. Solange die Variable nicht leer ist, passt ein
  Dateinamensmuster auch auf Namen, die mit `.` beginnen, sodass `rm *` dann
  auch `.profile` entfernt.
- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag wie
  `de-DE`), exportiert an jeden gestarteten Befehl.

## SEE ALSO

- `man`
