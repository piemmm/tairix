## NAME

chmod — Dateimodusbits ändern

## SYNOPSIS

`chmod [-cfRv] [--] MODE file...`

## DESCRIPTION

Ändert die Berechtigungsbits jedes Dateioperanden auf `MODE`, der
Reihe nach. `MODE` ist entweder ein absoluter Oktalwert (`644`,
`0755`, …), der die Berechtigungsbits vollständig ersetzt, oder eine
kommagetrennte Liste symbolischer Klauseln
`[ugoa]*([-+=]([rwxXst]*|[ugo]))+` oder `[-+=][0-7]+` (`g+w`, `o-rx`,
`a=rx`, `u+s`, `g=u`, `=644`), die die aktuellen Bits der Datei
umformen. Das symbolische `X` gewährt Ausführen nur einem Verzeichnis
oder einer Datei, die bereits ein Ausführungsbit trägt, `g=u` gibt der
Gruppe die Bits des Eigentümers, `s` setzt die set-ID-Bits des
Eigentümers und der Gruppe, und `t` setzt das Sticky-Bit. Eine Klausel,
die niemanden nennt (`+x`, `=r`), ändert jedes Feld außer den Bits, die
die umask enthält.

Bei einem Verzeichnis bleiben die set-user-ID- und set-group-ID-Bits,
wie sie sind, sofern der Modus sie nicht nennt: mit `s` (`g-s`), mit
Oktalziffern nach einem Operator (`=755`) oder mit einem Oktalwert aus
fünf oder mehr Ziffern (`00755`). Das set-group-ID-Bit eines
Verzeichnisses sorgt dafür, dass seine neuen Einträge seine Gruppe
erhalten; deshalb behält `chmod 755` es bei.

Nur der Eigentümer einer Datei darf ihren Modus ändern; der Kernel
weist jeden anderen ab, und der Besitz einer Capability gewährt kein
Vorrecht. Bei einer als Operand genannten symbolischen Verknüpfung wird
die Datei geändert, die sie benennt. Mit `-R` wird ein
Verzeichnisoperand geändert und danach sein Inhalt rekursiv; eine
innerhalb des Baums angetroffene symbolische Verknüpfung bleibt
unberührt, ebenso das, was sie benennt. Eine Datei, die sich nicht
erreichen oder ändern lässt, wird gemeldet, und die übrigen werden
trotzdem geändert. `--` beendet die Optionsauswertung: jedes spätere
Argument ist ein Operand. Ein mit `-` beginnender Modus wird ohne den
Strich geschrieben (`a-w`), oder die Optionen werden zuerst beendet
(`chmod -- -w file`).

## OPTIONS

- `-R, --recursive` — Dateien und Verzeichnisse rekursiv ändern.
- `-c, --changes` — nur Dateien melden, deren Modus sich tatsächlich
  geändert hat.
- `-v, --verbose` — jede verarbeitete Datei melden, auch eine, deren
  Änderung abgewiesen wurde oder die sich nicht erreichen ließ.
- `-f, --silent, --quiet` — die meisten Fehlermeldungen
  unterdrücken; der Lauf schlägt dennoch fehl, und der Exit-Status
  meldet es.
- `--preserve-root` — mit `-R` das Ändern von `/` verweigern, auf
  welchem Weg es auch erreicht wird.
- `--no-preserve-root` — `/` wie jedes andere Verzeichnis behandeln (der
  Standard).
- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `chmod 644 notes.txt` — Eigentümer lesen/schreiben, alle anderen
  nur lesen.
- `chmod g+w shared.txt` — der Gruppe Schreibrecht zu den aktuellen
  Bits hinzufügen.
- `chmod -R a=rx Docs` — den Baum `Docs` für alle lesbar und
  betretbar machen.
- `chmod +x build.sh` — ein Skript für alle ausführbar machen, die die
  umask zulässt.

## EXIT STATUS

- `0` — jede Modusänderung ist gelungen.
- `1` — eine Datei ließ sich nicht erreichen oder ändern, oder die
  Ausgabe schlug fehl; jeder Grund wird auf der Standardfehlerausgabe
  gemeldet (unter `-f` unterdrückt). Eine Befehlszeile, die nicht
  verstanden wurde oder deren Modusoperand weder oktal noch symbolisch
  war, endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag
  wie `de-DE`).

## SEE ALSO

- `ls`
- `mkdir`
- `rm`
