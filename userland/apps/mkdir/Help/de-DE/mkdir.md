## NAME

mkdir — Verzeichnisse anlegen

## SYNOPSIS

`mkdir [-pv] [-m MODE] [--] Verzeichnis...`

## DESCRIPTION

Legt jeden Verzeichnis-Operanden der Reihe nach an. Ohne `-p` muss das
Elternverzeichnis jedes Operanden bereits existieren und der Operand
selbst darf nicht existieren; ein Operand, der sich nicht anlegen lässt,
wird gemeldet, und der nächste wird trotzdem angelegt.

Mit `-p` wird jeder fehlende Vorfahr zuerst angelegt, vom äußersten zum
innersten, und ein Operand (oder Vorfahr), der bereits als Verzeichnis
existiert, ist kein Fehler. Ein Vorfahr, der als Datei existiert,
schlägt weiterhin fehl: nichts wird jemals stillschweigend ersetzt.

Ein neues Verzeichnis erhält den Modus `a=rwx` abzüglich der umask.
Mit `-m` erhält jeder angelegte Verzeichnis-Operand stattdessen
genau `MODE`, unabhängig von der umask: einen Oktalwert oder
symbolische Klauseln, wie `chmod` sie liest, ausgehend von `a=rwx`.
Ein fehlender Vorfahr, den `-p` anlegt, behält das Schreib- und
Suchrecht seines Eigentümers, was auch immer die umask besagt,
sodass sich das Verzeichnis darunter immer anlegen lässt. Ein
Verzeichnis, das in einem Set-Group-ID-Verzeichnis angelegt wird,
behält das geerbte Bit, sofern `MODE` es nicht nennt. Ein bereits
existierender Operand behält seinen Modus. `--` beendet die
Optionsanalyse: jedes spätere Argument ist ein Pfad.

## OPTIONS

- `-m, --mode=MODE` — jedem angelegten Verzeichnis-Operanden den Modus
  `MODE` geben, oktal oder symbolisch wie bei `chmod`, statt `a=rwx`
  abzüglich der umask.
- `-p, --parents` — fehlende Elternverzeichnisse anlegen; ein Operand,
  der bereits ein Verzeichnis ist, ist kein Fehler.
- `-v, --verbose` — jedes angelegte Verzeichnis als
  `mkdir: created directory 'Verz'` melden.
- `-h, -?` — die Kurzhilfe dieses Befehls anzeigen (auch `--help`).

## EXAMPLES

- `mkdir Notes` — ein Verzeichnis im aktuellen Verzeichnis anlegen.
- `mkdir -p Projects/os/build` — die ganze Kette anlegen und bereits
  vorhandene Teile überspringen.
- `mkdir -pv Home:/tools/bin` — unter einer Alias-Wurzel anlegen und
  jedes neue Verzeichnis melden.
- `mkdir -m 700 Private` — ein Verzeichnis anlegen, das nur sein
  Eigentümer benutzen darf.

## EXIT STATUS

- `0` — jedes Verzeichnis wurde angelegt (oder existierte mit `-p`
  bereits).
- `1` — ein Dateisystem- oder Ausgabefehler oder ein Modus, der sich
  nicht setzen ließ; der Grund wird auf der Standardfehlerausgabe
  gemeldet. Eine nicht verstandene Befehlszeile oder ein `MODE`, der
  kein Modus ist, endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Tag wie
  `de-DE`).

## SEE ALSO

chmod, rmdir, rm, ls
