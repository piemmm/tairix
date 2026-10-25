## NAME

chown — Dateieigentümer und -gruppe ändern

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

Ändert den Eigentümer und die Gruppe jedes Dateioperanden, der Reihe
nach. `OWNER` ändert den Benutzer und behält die Gruppe bei,
`OWNER:GROUP` ändert beide, und `:GROUP` ändert nur die Gruppe. `OWNER:`
gibt der Datei außerdem die Anmeldegruppe des Eigentümers. Ein leerer
Operand oder `:` allein ändert keines von beiden. Die ältere
Schreibweise `OWNER.GROUP` funktioniert mit einer Warnung, sofern nicht
der ganze Operand ein Konto benennt. Mit `--reference` erhält jede Datei
Eigentümer und Gruppe dessen, was `RFILE` benennt.

`OWNER` und `GROUP` sind Konto- und Gruppennamen oder Zahlen: Zuerst
wird ein Name versucht, und eine Zahl gilt nur, wenn nichts diesen Namen
trägt. Ein führendes `+` (`+1000`) bedeutet immer eine Zahl.

Um eine Datei einem anderen Konto oder einer Gruppe zu übergeben, der
Sie nicht angehören, brauchen Sie die Capability `CAP_FS_CHOWN`; ohne
sie dürfen Sie die Gruppe einer Datei, die Ihnen gehört, auf eine Ihrer
eigenen Gruppen ändern. Eine Änderung löscht das set-user-ID-Bit einer
Datei und ihr set-group-ID-Bit, wenn ihre Gruppe sie ausführen darf; ein
Verzeichnis behält beide.

Bei einer als Operand genannten symbolischen Verknüpfung wird die Datei
geändert, die sie benennt, oder mit `-h` die Verknüpfung selbst. Mit
`-R` werden ein Verzeichnisoperand und alles darin geändert, jedes
Verzeichnis nach seinem Inhalt. Eine innerhalb des Baums angetroffene
symbolische Verknüpfung wird selbst geändert, und das, was sie benennt,
wird nie berührt; mit `--dereference` bleibt auch die Verknüpfung
unberührt. Mit `-R` wird eine als Operand genannte symbolische
Verknüpfung selbst geändert und nicht verfolgt: `--dereference` ändert
stattdessen, was sie benennt, und `-H` durchläuft ein Verzeichnis, das
sie benennt. Eine Datei, die sich nicht erreichen oder ändern lässt,
wird gemeldet, und die übrigen werden trotzdem geändert. Eine Meldung
nennt jeden Eigentümer und jede Gruppe beim Namen, oder bei der Zahl,
wenn kein Konto und keine Gruppe sie hat. `--` beendet die
Optionsauswertung: jedes spätere Argument ist ein Operand.

## OPTIONS

- `-c, --changes` — nur Dateien melden, deren Eigentümer oder Gruppe
  sich tatsächlich geändert hat.
- `-f, --silent, --quiet` — die meisten Fehlermeldungen unterdrücken;
  der Lauf schlägt dennoch fehl, und der Exit-Status meldet es.
- `-v, --verbose` — jede verarbeitete Datei melden, auch eine, deren
  Änderung abgewiesen wurde oder die sich nicht erreichen ließ.
- `--dereference` — ändern, was eine symbolische Verknüpfung benennt,
  statt der Verknüpfung; ohne `-R` ist dies der Standard.
- `-h, --no-dereference` — eine symbolische Verknüpfung selbst ändern
  statt dessen, was sie benennt.
- `--preserve-root` — mit `-R` das Ändern von `/` verweigern, auf
  welchem Weg es auch erreicht wird.
- `--no-preserve-root` — `/` wie jedes andere Verzeichnis behandeln (der
  Standard).
- `--from=CURRENT_OWNER:CURRENT_GROUP` — eine Datei nur ändern, wenn ihr
  aktueller Eigentümer und ihre aktuelle Gruppe diese sind; ein
  weggelassener Teil lässt jeden Wert zu.
- `--reference=RFILE` — jeder Datei Eigentümer und Gruppe von `RFILE`
  geben, statt sie zu nennen.
- `-R, --recursive` — Dateien und Verzeichnisse rekursiv ändern.
- `-H` — mit `-R` das Verzeichnis durchlaufen, zu dem eine als Operand
  genannte symbolische Verknüpfung führt.
- `-L` — mit `-R` jeder symbolischen Verknüpfung zu einem Verzeichnis
  folgen; abgelehnt, weil eine rekursive Änderung nie einer Verknüpfung
  innerhalb des Baums folgt.
- `-P` — mit `-R` keiner symbolischen Verknüpfung folgen (der Standard).
  Von `-H`, `-L` und `-P` gilt das zuletzt angegebene.
- `-?, --help` — die Kurzhilfe dieses Befehls anzeigen.
- `--version` — Namen und Version des Werkzeugs ausgeben.

## EXAMPLES

- `chown alice notes.txt` — `notes.txt` an `alice` übergeben und die
  Gruppe beibehalten.
- `chown alice:staff notes.txt` — die Datei an `alice` und die Gruppe
  `staff` übergeben.
- `chown :staff shared.txt` — nur die Gruppe ändern.
- `chown -R alice Projects` — den Baum `Projects` an `alice` übergeben.
- `chown -R --from=bob alice Shared` — `alice` nur das übergeben, was
  `bob` in `Shared` gehört.
- `chown --reference=template.txt notes.txt` — `notes.txt` Eigentümer
  und Gruppe von `template.txt` geben.

## EXIT STATUS

- `0` — jede Änderung ist gelungen.
- `1` — eine Datei ließ sich nicht erreichen oder ändern, oder die
  Ausgabe schlug fehl; jeder Grund wird auf der Standardfehlerausgabe
  gemeldet (unter `-f` unterdrückt). Wurde die Befehlszeile nicht
  verstanden oder ließ sich ein Eigentümer, eine Gruppe oder eine
  Referenzdatei nicht lesen, endet der Lauf ebenfalls mit `1` und ändert
  nichts.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag
  wie `de-DE`).

## SEE ALSO

- `chmod`
- `ls`
- `stat`
