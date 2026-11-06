## NAME

ls — Verzeichnisinhalte auflisten

## SYNOPSIS

`ls [-aABbCcdFfGghikIlmNnopQqrRsSTtUuvXx1] [-w cols] [-I PATTERN]`
`[--block-size=SIZE] [--si] [--format=WORD] [--indicator-style=WORD]`
`[--hide=PATTERN] [--time=WORD] [--time-style=STYLE] [--sort=WORD]`
`[--quoting-style=STYLE] [--full-time] [--author] [--file-type]`
`[--group-directories-first] [--zero] [--color[=WHEN]] [--] [path...]`

## DESCRIPTION

Listet jeden Pfad-Operanden auf: die Einträge eines
Verzeichnis-Operanden werden gelesen und aufgelistet (außer `-d`
benennt das Verzeichnis selbst), jeder andere Operand wird als er
selbst gelistet. Ohne Operand wird das aktuelle Verzeichnis (`.`)
gelistet.

Einträge werden nach Namen sortiert (oder mit `-S` nach Größe, die
größte zuerst; mit `-t` nach Zeitstempel, den neuesten zuerst; mit `-X`
nach Endung; mit `-v` in natürlicher „Versions“-Reihenfolge; mit `-U`
gar nicht — in Verzeichnisreihenfolge; mit `--sort` nach Name gewählt;
mit `-r` umgekehrt). `--group-directories-first` hebt Verzeichnisse an
den Anfang der Sortierung. Ist die Ausgabe ein Terminal, werden sie in
Spalten angeordnet, die auf die Terminalbreite bemessen und von oben nach
unten gefüllt sind (`-C`); ist sie keines (eine Pipe oder eine Datei),
wird ein Name pro Zeile aufgelistet. `-x` füllt die Spalten stattdessen
von links nach rechts, `-m` listet sie durch Kommas getrennt, `-1`
erzwingt einen pro Zeile, und `-w` legt die Breite ausdrücklich fest.
Einträge, deren Name mit `.` beginnt, werden ausgeblendet, sofern nicht
`-a` oder `-A` angegeben ist; werden Einträge ausgeblendet, erscheint ein
Hinweis auf dem Standard-Informationsstrom (fd 3), niemals in der Liste
selbst.

Das lange Format (`-l`) zeigt die Typ- und Berechtigungsbits, den
Besitzer und die Gruppe, die Größe, einen Zeitstempel und dann den
Namen. Besitzer und Gruppe sind numerische Ids: das Auflösen von
Kontonamen erfordert die fähigkeitsgeschützte Benutzerdatenbank, die
eine Auflistung nicht verlangen darf; die Ausgabe entspricht daher dem
numerischen Rückfall des GNU-Werkzeugs (`-n` liefert dasselbe). Der
Zeitstempel ist standardmäßig die Änderungszeit; `-c`, `-u` und
`--time` wählen, welcher der vier Zeitstempel angezeigt (und wonach
sortiert) wird, und `--time-style` — oder `--full-time` — legt sein
Format fest. Die Spalte der Link-Anzahl gibt an, wie viele
Verzeichniseinträge die Datei benennen, so wie das Dateisystem selbst es
festhält; ein Format, das keine Anzahl führt, antwortet `1`.

Eine symbolische Verknüpfung erscheint mit dem Typbuchstaben `l` und im
Langformat als `name -> target` — das Ziel genau so, wie es gespeichert
ist, unaufgelöst, denn das ist der Inhalt der Verknüpfung. Eine ins Leere
zeigende Verknüpfung wird daher normal aufgelistet; nur eine auflösende
Haltung (`-L`, oder `-H` für einen Operanden) meldet ein unerreichbares
Ziel.

Namen werden angeführt, damit störende Zeichen sichtbar sind und sich
gefahrlos wieder in eine Shell einfügen lassen: am Terminal ist der
Standard `shell-escape` (ein Name mit Leer- oder Metazeichen wird
angeführt, Steuerzeichen erscheinen als `$'…'`-Escapes), sonst `literal`
(Namen unverändert). `-N`, `-Q`, `-b` und `--quoting-style` wählen den
Stil, und `-q` zeigt Steuerzeichen als `?`. Unter `-F` und `--file-type`
wird ein Name, der `*`, `=`, `>`, `@` oder `|` enthält, von den
Shell-Stilen angeführt, und `-Q` und `-b` maskieren dieses Zeichen
darin, sodass er nicht als Kennzeichnungssuffix durchgehen kann. GNUs
`-F` lässt einen `*` unmarkiert, weshalb dort ein darauf endender Name
wie eine ausführbare Datei wirkt.

An einem Farbterminal werden Namen nach Art eingefärbt — Verzeichnisse,
ausführbare Dateien und einfache Dateien jeweils in der Rolle des
Standardschemas —, gesteuert durch `--color`. Farbe ist reine
Darstellung: sie kommt nur zur Terminalausgabe hinzu und nie zu
weitergeleiteter oder umgeleiteter Ausgabe, und abgesehen von den
Escape-Sequenzen ist die Ausgabe Byte für Byte mit der einfachen
Auflistung identisch, sodass Farbe nie Spalten, Reihenfolge oder die
Sicht eines Skripts ändert.

Bei mehreren Operanden — und stets unter `-R` — wird jeder
Verzeichnisliste eine `path:`-Kopfzeile vorangestellt, und Blöcke werden
durch eine Leerzeile getrennt. Der Pfad einer Kopfzeile erhält die
Anführung im Stil der Auflistung, ebenso jeder `:` darin.

## OPTIONS

- `-t` — nach dem angezeigten Zeitstempel sortieren, den neuesten
  zuerst.
- `-c` — die Metadaten-Änderungszeit (ctime) verwenden: mit `-l`
  anzeigen und mit `-t` danach sortieren; ohne `-l` danach sortieren.
- `-u` — wie `-c`, aber die Zugriffszeit (atime).
- `-i, --inode` — die Knotennummer jedes Eintrags ausgeben.
- `-B, --ignore-backups` — Einträge, deren Name mit `~` endet, nicht
  auflisten, in jedem Modus (Sicherungen sind auch unter `-a` verborgen).
- `-I, --ignore=PATTERN` — Einträge, die auf das Shell-Glob `PATTERN`
  passen, nicht auflisten (wiederholbar); gilt in jedem Modus. Ein
  führender `.` wird nur von einem `.` im Muster getroffen, nie von einem
  Platzhalter.
- `--hide=PATTERN` — wie `--ignore`, aber ohne Wirkung, wenn `-a` oder
  `-A` angegeben ist.
- `--time=WORD` — welcher Zeitstempel angezeigt und wonach sortiert
  wird: `atime` (`access`, `use`), `ctime` (`status`), `mtime`
  (`modification`) oder `birth` (`creation`).
- `--time-style=STYLE` — Zeitstempelformat: `locale` (Standard),
  `long-iso`, `full-iso` oder `iso`. Ein eigenes `+FORMAT` wird nicht
  unterstützt.
- `--full-time` — wie `-l --time-style=full-iso`.
- `-a, --all` — Einträge, deren Name mit `.` beginnt, nicht
  ausblenden.
- `-A, --almost-all` — wie `-a`, aber `.` und `..` niemals auflisten.
- `-d, --directory` — Verzeichnis-Operanden selbst auflisten, nicht
  ihren Inhalt.
- `-F, --classify` — `/` an Verzeichnisse und `*` an ausführbare
  Dateien anhängen.
- `-g` — langes Format ohne die Besitzerspalte; impliziert `-l`.
- `-h, --human-readable` — Größen wie `1.1K`, `23M` ausgeben (Potenzen
  von 1024), sowohl für die Dateigrößen des Langformats als auch für die
  `-s`-Blöcke.
- `-l` — langes Format: Berechtigungsbits, Besitzer, Gruppe, Größe,
  dann Name.
- `-m` — durch Kommas getrennte Namen, auf die Ausgabebreite umgebrochen.
- `-n, --numeric-uid-gid` — langes Format mit numerischem Besitzer
  und numerischer Gruppe; impliziert `-l`. Besitzer und Gruppe sind
  hier immer numerisch (siehe oben), also identisch zu `-l`.
- `-o` — langes Format ohne die Gruppenspalte; impliziert `-l`.
- `-p` — `/` an Verzeichnisse anhängen.
- `-N, --literal` — Namen unverändert ausgeben, ohne Anführung
  (`--quoting-style=literal`).
- `-Q, --quote-name` — Anführung im C-Stil: jeden Namen in doppelte
  Anführungszeichen setzen; Anführungszeichen, Backslashes und
  Steuerzeichen werden maskiert (`--quoting-style=c`).
- `-b, --escape` — wie `-Q`, aber ohne die umgebenden
  Anführungszeichen und mit maskierten Leerzeichen
  (`--quoting-style=escape`).
- `--quoting-style=WORD` — wie Namen angeführt werden: `literal`
  (`-N`), `shell`, `shell-always`, `shell-escape`,
  `shell-escape-always`, `c` (`-Q`) oder `escape` (`-b`). Standard ist
  `shell-escape` am Terminal und sonst `literal`; die Stile `locale`
  und `clocale` werden nicht unterstützt.
- `-q, --hide-control-chars` — nichtdruckbare Zeichen als `?` anzeigen
  (Standard am Terminal); betrifft nur die nicht maskierenden Stile.
- `--show-control-chars` — nichtdruckbare Zeichen unverändert ausgeben
  (Standard, wenn die Ausgabe kein Terminal ist).
- `-r, --reverse` — die Sortierreihenfolge umkehren.
- `-R, --recursive` — Unterverzeichnisse rekursiv auflisten.
- `-L, --dereference` — Angaben zu der Datei zeigen, die jede symbolische
  Verknüpfung benennt, statt zur Verknüpfung selbst, wo immer eine
  auftritt. Eine Verknüpfung, deren Ziel nicht erreichbar ist, wird auf der
  Standardfehlerausgabe gemeldet, die Auflistung läuft weiter und der
  Exit-Status ist ungleich null.
- `-H, --dereference-command-line` — nur die auf der Befehlszeile genannten
  symbolischen Verknüpfungen auflösen; Verknüpfungen innerhalb einer
  Auflistung bleiben Verknüpfungen. Die spätere von `-L` und `-H` gewinnt.
- `--dereference-command-line-symlink-to-dir` — der Standard, solange keine
  Formatoption etwas anderes erzwingt: eine Verknüpfung der Befehlszeile
  *auf ein Verzeichnis* wird aufgelöst, sodass `ls linkdir` das Verzeichnis
  auflistet, während jede andere Verknüpfung sich selbst zeigt. `-l`, `-d`
  und `-F` zeigen stattdessen standardmäßig jede Verknüpfung selbst.
- `-s, --size` — die belegte Größe jedes Eintrags in Blöcken ausgeben
  (skaliert durch `-h` / `--si` / `--block-size` / `-k`), mit einer
  `total`-Zeile je aufgelistetem Verzeichnis.
- `-C` — Einträge in Spalten auflisten, von oben nach unten gefüllt
  (Standard, wenn die Ausgabe ein Terminal ist).
- `-S` — nach Größe sortieren, die größte zuerst.
- `-U` — nicht sortieren; Einträge in Verzeichnisreihenfolge auflisten.
- `-X` — nach Dateiendung (Text ab dem letzten `.`) sortieren, bei
  Gleichstand nach Namen.
- `-v` — natürliche „Versions“-Sortierung, sodass `f2` vor `f10`
  steht; bei Gleichstand nach Namen.
- `-f` — nicht sortieren und alle Einträge zeigen: aktiviert `-a` und
  `-U` und deaktiviert `-l` und `-s`. Wirkt an seiner Position, sodass
  ein späteres `-l`/`-s`/Sortier-Flag es überschreibt.
- `--sort=WORD` — Sortierschlüssel nach Name wählen: `none` (`-U`),
  `size` (`-S`), `time` (`-t`), `version` (`-v`), `extension` (`-X`)
  oder `name`.
- `--group-directories-first` — Verzeichnisse vor anderen Einträgen
  auflisten; Verzeichnisse zuerst, auch mit `-r`.
- `-w, --width <cols>` — die Ausgabebreite in Spalten festlegen; `0`
  bedeutet eine unbegrenzte Zeilenlänge. Ohne diese Option wird die
  Breite des Terminals verwendet, oder 80, wenn sie sich nicht ermitteln
  lässt.
- `-x` — Einträge in Spalten auflisten, von links nach rechts gefüllt.
- `-1` — ein Name pro Zeile (der Standard, wenn die Ausgabe kein Terminal
  ist).
- `-?` — die Kurzhilfe dieses Befehls anzeigen (`--help` ist die
  lange Form).
- `--file-type` — `/` an Verzeichnisse anhängen, aber nie `*` an
  ausführbare Dateien (`--indicator-style=file-type`).
- `--indicator-style=WORD` — das Kennzeichnungssuffix nach Name wählen:
  `none`, `slash` (`-p`), `file-type` (`--file-type`) oder `classify`
  (`-F`).
- `-G, --no-group` — die Gruppenspalte im Langformat weglassen; wählt
  anders als `-o` nicht selbst das Langformat.
- `--author` — mit `-l` die Autorspalte (den besitzenden Benutzer, da es
  keinen eigenen Autor gibt) nach dem Besitzer und vor der Gruppe
  ausgeben.
- `--si` — wie `-h`, aber Potenzen von 1000 (`1.1k`, `23M`).
- `-k, --kibibytes` — 1024-Byte-Blöcke für die `-s`-Zellen und die
  `total`-Zeile verwenden. Das ist bereits der Standard, daher bestätigt
  es die Ausgabe, statt sie zu ändern; eine Größenoption hat Vorrang.
- `--block-size=SIZE` — die Dateigrößen des Langformats und die
  `-s`-Blöcke um SIZE skalieren: eine einfache ganze Zahl (Bytes) oder
  eine Einheit `K`/`M`/`G`/`T`/`P`/`E` (auf 1024 bezogen), eine Einheit
  im Stil von `KiB` (auf 1024 bezogen) oder im Stil von `KB` (auf 1000
  bezogen), optional mit vorangestelltem ganzzahligem Koeffizienten. Eine
  bloße Einheit gibt ihr Suffix aus; ein Koeffizient unterdrückt es. Ein
  fehlerhaftes SIZE ist ein Fehler.
- `--format=WORD` — die Anordnung nach Name wählen: `long` (`-l`) oder
  `verbose`, `single-column` (`-1`), `vertical` (`-C`), `across` oder
  `horizontal` (`-x`) oder `commas` (`-m`).
- `-T, --tabsize <cols>` — den Tabulatorschritt des Spaltengitters
  setzen (Standard 8); `0` füllt nur mit Leerzeichen.
- `--zero` — jede Ausgabezeile mit NUL statt Zeilenumbruch beenden;
  wählt außerdem, sofern nicht überschrieben, die Einzelspalte, wörtliche
  Anführung und sichtbare Steuerzeichen.

- `--color[=WHEN]` — Namen nach Art einfärben (Verzeichnisse,
  ausführbare Dateien, einfache Dateien). `WHEN` ist `auto` (die
  Vorgabe: nur einfärben, wenn die Ausgabe ein bestätigtes Terminal
  ist), `always` (auch sonst einfärben, z. B. eine serielle Konsole)
  oder `never`; `--color` ohne `WHEN` bedeutet `always`. Weitergeleitete
  oder umgeleitete Ausgabe wird nie eingefärbt.

## EXAMPLES

- `ls` — das aktuelle Verzeichnis auflisten.
- `ls -al /System` — Auflistung von `/System` im langen Format,
  einschließlich ausgeblendeter Einträge.
- `ls -lhS` — langes Format, lesbare Größen, die größte zuerst.
- `ls -v` — natürliche Sortierung, sodass `f2` vor `f10` kommt.
- `ls --group-directories-first` — zuerst Verzeichnisse, dann Dateien.
- `ls -R Documents` — `Documents` rekursiv durchlaufen, eine
  Kopfzeile pro Verzeichnis.
- `ls -F` — Verzeichnisse mit `/` und ausführbare Dateien mit `*`
  markieren.
- `ls -d Documents` — den Eintrag `Documents` selbst auflisten, nicht
  seinen Inhalt.

## EXIT STATUS

- `0` — jeder Operand wurde aufgelistet.
- `1` — ein kleines Problem: ein Eintrag oder ein Unterverzeichnis
  innerhalb einer Auflistung konnte nicht untersucht oder gelesen
  werden.
- `2` — ein ernstes Problem: die Befehlszeile wurde nicht verstanden,
  ein Operand konnte nicht untersucht oder gelesen werden, `-R`
  erreichte ein Verzeichnis über eine Verknüpfung erneut, der Speicher
  ging aus, oder die Ausgabe konnte nicht zugestellt werden.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag
  wie `de-DE`).

- `TERM` — der Terminaltyp, der die Farbtiefe der `--color`-Ausgabe
  bestimmt. Ein nicht gesetztes oder farbloses `TERM` gibt unter `auto`
  einfachen Text aus.

## SEE ALSO

- `cat`
- `man`
