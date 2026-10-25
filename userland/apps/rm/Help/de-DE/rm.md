## NAME

rm — Dateien und Verzeichnisse entfernen

## SYNOPSIS

`rm [-dfiIrRv] [--] file...`

## DESCRIPTION

Entfernt jeden Dateioperanden, der Reihe nach. Ein Operand, der kein
Verzeichnis ist, wird entlinkt — eine symbolische Verknüpfung wird
selbst entfernt, nie das, was sie benennt; ein Verzeichnisoperand wird
nur mit `-r` entfernt (das den Inhalt tiefenzuerst und dann das
Verzeichnis selbst entfernt) oder, wenn er leer ist, mit `-d`. In beiden
Fällen wird ein Operand, dessen letzte Komponente `.` oder `..` ist,
abgelehnt, wie es POSIX verlangt.

Mit `-f` wird ein nicht vorhandener Operand stillschweigend
übersprungen und nie eine Frage gestellt. `-i` fragt auf der
Standardfehlerausgabe vor jeder Entfernung, wobei es nennt, was es
entfernen würde (`remove regular empty file 'a'?`,
`remove symbolic link 'l'?`), und vor dem Abstieg in ein Verzeichnis,
das etwas enthält; `-I` fragt einmal vorab, bevor mehr als drei
Operanden oder rekursiv entfernt wird. Eine abgelehnte Frage lässt das
Objekt bestehen (oder überspringt den ganzen Lauf, bei `-I`) ohne
Fehler; eine unlesbare Antwort gilt nie als Zustimmung. Das spätere von
`-f`, `-i` und `-I` gewinnt.

Ohne `-f` wird, wenn die Standardeingabe ein Terminal ist, vor dem
Entfernen einer Datei, die der Aufrufer nicht schreiben darf, gefragt
(`remove write-protected regular file 'x'?`), und sie wird nur bei einer
mit `y` oder `Y` beginnenden Antwort entfernt; bei einer symbolischen
Verknüpfung wird nie gefragt. Unter `-i` nennt die Frage auch den
Schutz.

Ein Pfad, der sich nicht untersuchen, lesen oder entfernen lässt, wird
gemeldet, und die übrigen werden trotzdem entfernt; ein Verzeichnis, in
dem noch etwas verbleibt, wird stillschweigend behalten. Ein rekursives
Entfernen von `/` wird, in welcher Schreibweise auch immer, unter
`--preserve-root`, dem Standard, abgelehnt. `--` beendet die
Optionsauswertung: jedes spätere Argument ist ein Pfad.

## OPTIONS

- `-r, -R, --recursive` — Verzeichnisse mitsamt Inhalt entfernen.
- `-f, --force` — nicht vorhandene Operanden ignorieren; nie fragen.
- `-d, --dir` — leere Verzeichnisse entfernen.
- `-i, --interactive` — vor jeder Entfernung fragen; nur eine mit
  `y`/`Y` beginnende Antwort stimmt zu.
- `-I` — einmal fragen, bevor mehr als drei Operanden oder rekursiv
  entfernt wird.
- `-v, --verbose` — jede Entfernung als `removed 'file'` melden.
- `--preserve-root` — das rekursive Entfernen von `/` verweigern (der
  Standard).
- `--no-preserve-root` — das Entfernen von `/` erlauben. Die Option wird
  nur vollständig ausgeschrieben angenommen.
- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `rm notes.txt` — eine Datei entfernen.
- `rm -r Scratch` — den Baum `Scratch` samt Inhalt entfernen.
- `rm -I a b c d` — einmal fragen, dann bei `y` alle vier Dateien
  entfernen.

## EXIT STATUS

- `0` — jede Entfernung ist gelungen (eine abgelehnte Frage und ein
  `-f`-Überspringen sind keine Fehlschläge).
- `1` — ein Pfad ließ sich nicht entfernen, oder das Fragen oder eine
  Ausgabe schlug fehl; jeder Grund wird auf der Standardfehlerausgabe
  gemeldet. Eine nicht verstandene Befehlszeile endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag
  wie `de-DE`).

## SEE ALSO

- `cp`
- `ls`
- `mv`
