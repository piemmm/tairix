## NAME

TextEdit — grafischer Text- und Hex-Editor

## SYNOPSIS

`TextEdit`

## DESCRIPTION

Bearbeitet jede Datei in einem Desktop-Fenster: Text, Quelltext, die
Einstellungsdateien des Systems oder rohe Bytes. Mit einem Dokument gestartet
— aus der Dateiverwaltung, vom Desktop oder indem eine Datei auf sein Symbol
in der Symbolleiste gezogen wird — öffnet er ein Fenster auf diese Datei.
Allein gestartet öffnet er ein leeres Fenster. Jedes Dokument ist ein Fenster
des einen Editors; wird das letzte geschlossen, bleibt er in der
Symbolleiste, und die Zeile „Beenden“ seines Symbolmenüs beendet ihn.

Nichts, was eine Datei enthält, bleibt verborgen. Ein Steuerbyte erscheint
als `[x03]`, ein Byte, das kein gültiges UTF-8 ist, als `[xC3]`, und ein
unsichtbares oder die Schreibrichtung änderndes Zeichen als `[U+202E]`, jedes
in eigener Farbe und jedes ein Schritt der Einfügemarke. Eine Datei, die nach
Binärdaten aussieht, öffnet sich in der Hex-Ansicht, die jedes Byte als zwei
Hex-Ziffern neben seinem Zeichen zeigt und dieselben Bytes bearbeitet wie die
Textansicht.

Quelltexte werden eingefärbt: HTML, XML und SVG, CSS, JavaScript, JSON, YAML,
TOML, Markdown, Rust, C, Java, Python und Shell-Skripte. Die
Einstellungsdateien des Systems — Anwendungseinstellungen, die
Programmbibliothek, die System- und Netzwerkkonfiguration, die
Dienstüberschreibungen, die Benutzer- und Gruppendatenbanken und die
Manifeste von Schriftfamilien — werden ebenfalls eingefärbt und mit dem
Parser geprüft, mit dem das System sie liest: Ein Problem wird am Rand neben
seiner Zeile markiert und in der Statuszeile genannt. Das Format wird nach
dem Dateinamen gewählt, sonst nach den ersten Bytes; ein im Menü „Ansicht“
oder in der Statuszeile gewähltes Format hat stets Vorrang.

Der Editor besitzt keine Berechtigung für das Dateisystem. Er bearbeitet nur
die Datei, die ihm übergeben wurde. Eine Datei, die der Benutzer ändern darf,
wird beschreibbar übergeben, und „Sichern“ schreibt sie zurück; jede andere
ist schreibgeschützt, und „Sichern“ fragt, wohin eine Kopie gesichert werden
soll. Einfärben, Formaterkennung und Prüfung laufen in einem eigenen
Arbeitsprozess ohne jeden Zugriff, sodass eine feindselige Datei nichts
erreicht, was der Editor erreicht.

Das Drücken der sekundären (rechten) Maustaste an einer beliebigen Stelle
im Fenster öffnet dessen Menü: Ausschneiden, Kopieren, Einfügen und Alles
auswählen, darunter „Datei“, „Bearbeiten“, „Suchen“ und „Ansicht“, die
jeweils ihr eigenes Untermenü öffnen. Das Fenster hat keine Menüleiste.

Die Statuszeile zeigt Zeile und Spalte der Einfügemarke, was die Prüfung
gefunden hat, und als Felder, die beim Anklicken ein Menü öffnen: das
Format, Text oder Hex, die Zeilenenden und die Einrückung. Wer ein Fenster
schließt oder beendet, ohne Änderungen gesichert zu haben, wird vorher
gefragt.

* `Ctrl+N` — ein neues Fenster
* `Ctrl+O` — eine Datei öffnen
* `Ctrl+S` — sichern; `Ctrl+Shift+S` — sichern unter
* `Ctrl+W` — das Fenster schließen
* `Ctrl+Z` — rückgängig; `Ctrl+Shift+Z` oder `Ctrl+Y` — wiederholen
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — ausschneiden, kopieren, einfügen
* `Ctrl+A` — alles auswählen
* `Ctrl+F` — suchen; `Ctrl+H` — ersetzen
* `F3` / `Shift+F3` — der nächste oder vorige Treffer
* `Ctrl+L` — zu einer Zeile springen
* `F8` — das nächste Problem, das die Prüfung gefunden hat
* `Ctrl+]` / `Ctrl+[` — die gewählten Zeilen ein- oder ausrücken
* `Ctrl+/` — die gewählten Zeilen aus- oder wieder einkommentieren
* `Ctrl+Shift+H` — zwischen Text- und Hex-Ansicht wechseln
* `Insert` — zwischen Einfügen und Überschreiben wechseln
* `Tab` — in der Hex-Ansicht zwischen Hex- und Zeichenspalte wechseln

## OPTIONS

`-h`, `-?`, `--help`
: Diese Hilfe auf die Standardausgabe schreiben und beenden.

## EXIT STATUS

Null nach „Beenden“. Nicht null, wenn der Fensterkanal, das Ereignispostfach
oder die Desktop-Sitzung abgelehnt wurde; der Grund wird auf der
Standardfehlerausgabe genannt.
