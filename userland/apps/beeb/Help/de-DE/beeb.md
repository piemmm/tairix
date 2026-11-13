## NAME

beeb — Emulator des BBC Micro Model B

## SYNOPSIS

`beeb`

## DESCRIPTION

Führt einen BBC Micro Model B in einem Desktop-Fenster aus, Takt für Takt in
seiner eigenen Geschwindigkeit, mit seinem eigenen, im Reinraumverfahren
geschriebenen MOS, BBC BASIC und DFS, sodass er an der BASIC-Eingabe mit
einem Diskettendateisystem startet. Das Bild ist das eines Fernsehers, vier
Einheiten breit auf drei hoch, und der Ton erklingt über das Audio des
Desktops.

Ein Diskettenabbild (`.ssd`, `.dsd`) oder Bandabbild (`.uef`, `.csw`), das
mit dem Emulator geöffnet, auf sein Fenster gezogen oder in seinem Menü mit
Diskette einlegen oder Band einlegen gewählt wird, kommt ins Laufwerk oder in
den Kassettenrekorder. Was die Maschine auf eine Diskette schreibt, geht in
die Datei der Diskette zurück, solange diese Datei beschrieben werden darf;
eine Diskette, deren Datei nicht beschrieben werden darf, ist
schreibgeschützt. Auswerfen nimmt eine Diskette heraus, und Diskette sichern
unter legt überall eine Kopie ab. Ein bespieltes Band wird gesichert, wenn es
herausgenommen wird. Schnappschuss sichern hält die ganze Maschine in einer
Datei fest, und Schnappschuss wiederherstellen holt sie samt Disketten und
Band zurück.

Das Menü, per Rechtsklick auf das Bild oder über das Symbol des Emulators in
der Symbolleiste geöffnet, bietet außerdem BREAK, Zurücksetzen, Pause und die
Geschwindigkeit; Vollbild, scharfe oder weiche Skalierung und Zeilenlinien;
die Belegung der Tastatur; und Einfügen, das den Text der Zwischenablage
tippt. Nach Zeichen tippen die Tasten, was sie zeigen, wobei die CAPS LOCK der
Maschine die Groß- und Kleinschreibung eines Buchstabens bestimmt. Nach
Position ist jede Taste die BBC-Taste an ihrem Platz: F1 bis F9 sind f1 bis
f9, F10 ist f0, F12 ist BREAK, Ende ist COPY und Pos1 ist SHIFT LOCK.

Einstellungen wählt, was der Betriebssystemsockel und die sechzehn
Seitensteckplätze enthalten — die mitgelieferten ROMs, Seiten-RAM oder eigene
ROM-Abbilder, im Speicher des Emulators installiert —, den Bildschirmmodus,
den Start und die Diskettenzeiten der Tastaturbrücken, das Bild, die Tasten,
die Lautstärke und das schnelle Laden vom Band, das die Maschine so schnell
wie möglich laufen lässt, solange der Kassettenmotor läuft. Änderungen an den
ROMs und den Brücken wirken beim nächsten Zurücksetzen.

Lampen am unteren Rand des Fensters zeigen CAPS LOCK, SHIFT LOCK, den
Kassettenmotor und jedes Laufwerk. Die Maschine läuft in einem Arbeitsprozess
ohne jede Befugnis, sodass weder ihrer Software noch ihren Medien vertraut
wird; ein Arbeitsprozess, der versagt, wird ersetzt, die Maschine startet neu
wie nach dem Einschalten, und der Grund wird angezeigt.

## OPTIONS

`-h`, `-?`, `--help`
: Schreibt diese Hilfe auf die Standardausgabe und beendet sich.

## EXIT STATUS

Null, sobald sein Fenster geschlossen oder im Menü seines Symbols in der Symbolleiste Beenden gewählt wird. Ungleich null, wenn der Fensterkanal, das Ereignispostfach oder die Desktop-Sitzung verweigert wurde; der Grund steht auf der Standardfehlerausgabe.
