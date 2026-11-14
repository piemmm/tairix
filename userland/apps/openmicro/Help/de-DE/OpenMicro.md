## NAME

OpenMicro — Open Micro Emulator, Emulator für 8-Bit-Mikrocomputer

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Führt 8-Bit-Mikrocomputer in Desktop-Fenstern aus — den BBC Micro Model B und
den Acorn Electron —, Takt für Takt in ihrer eigenen Geschwindigkeit, jeden mit
seinem eigenen, im Reinraumverfahren geschriebenen Betriebssystem und BASIC
ausgestattet und den Model B zudem mit einem Diskettendateisystem, sodass ein
Rechner an der BASIC-Eingabe startet. Das Bild ist das eines Fernsehers, vier
Einheiten breit auf drei hoch, und der Ton erklingt über das Audio des
Desktops.

Das Bibliotheksfenster zeigt jeden Rechner als Kachel mit seinem letzten
Bildschirm. Neu legt einen Rechner unter dem gewünschten Namen an: einen BBC
Micro Model B, mit einer neuen, leeren Diskette in Laufwerk 0 oder ohne, oder
einen Acorn Electron, mit angebautem Plus 1 oder ohne. Starten, ein Doppelklick
oder die Eingabetaste startet den gewählten Rechner in einem eigenen Fenster
oder holt dessen Fenster nach vorn; bis zu zwanzig Rechner werden aufbewahrt,
und mehrere laufen zugleich. Duplizieren kopiert einen Rechner samt seinem
gesicherten Zustand und seinem Ordner, und Löschen löscht einen Rechner und
seinen gesicherten Zustand, sobald das bestätigt ist, lässt seinen Ordner aber
bestehen. Ein Rechtsklick auf die Kachel eines Rechners bietet Starten,
Einstellungen, Duplizieren und Löschen für diesen Rechner an und, solange er
läuft, auch das Menü des Rechners. Beim ersten Start legt der Emulator einen
BBC Micro Model B mit einer leeren Diskette an.

Einstellungen ändert den Namen eines Rechners, seine Lautstärke und das
schnelle Laden vom Band, das den Rechner so schnell wie möglich laufen lässt,
solange der Kassettenmotor läuft. Was in ihn eingebaut ist, lässt sich nur
ändern, solange er heruntergefahren ist: das ROM in jedem Sockel — OS und
sechzehn Seitensteckplätze beim Model B, OS und BASIC beim Electron, ROM und
Steckmodule bei seinem Plus 1 —, jeweils ein mitgeliefertes ROM, Seiten-RAM in
einem Steckplatz des Model B, nichts in einem Steckplatz oder Steckmodul oder
ein eigenes ROM-Abbild, das mit Abbild installieren im Speicher des Emulators
abgelegt wurde; das Plus 1 des Electron; und die Tastaturbrücken des Model B,
die seinen Bildschirmmodus beim Start, das Starten von Diskette und die
Diskettenzeiten festlegen.

Das Schließen des Fensters eines Rechners versetzt den Rechner in den
Ruhezustand: wieder gestartet, macht er dort weiter, wo er stehen blieb.
Herunterfahren in seinem Menü nimmt seine Disketten und sein Band heraus,
schaltet ihn aus und behält nur seinen letzten Bildschirm.

Jeder Rechner hat in Ihrem Ordner Documents einen eigenen Ordner: Open Micro
und darin der Name des Rechners. Neue Diskette und Neues Band erstellen dort
eine leere Diskette oder ein leeres Band und legen es ein, und Aus dem Ordner
bietet die Disketten und Bänder an, die schon dort liegen. Was seine Laufwerke
und sein Kassettenrekorder aus seinem Ordner enthalten, bleibt beim Rechner:
heruntergefahren, legt er es beim Start wieder ein; aus dem Ruhezustand
fortgesetzt, schreibt er nur dorthin zurück, wo seitdem kein anderes Programm
etwas geändert hat.

Ein Diskettenabbild (`.ssd`, `.dsd`) oder Bandabbild (`.uef`, `.csw`), das mit
dem Emulator geöffnet wird, geht an den laufenden Rechner, dessen Fenster
zuletzt aktiv war, oder an einen anderen laufenden Rechner, der es aufnimmt,
oder sonst an den ersten Rechner der Bibliothek, der es aufnimmt und dafür
gestartet wird. Eines, das mit Diskette einlegen oder Band einlegen gewählt
wird, kommt in das betreffende Laufwerk oder in den Kassettenrekorder, und
eines, das auf das Fenster eines Rechners gezogen wird, kommt schreibgeschützt
in Laufwerk 0 oder in den Rekorder. Was der Rechner auf eine Diskette schreibt,
geht in die Datei der Diskette zurück, solange diese Datei beschrieben werden
darf; eine Diskette, deren Datei nicht beschrieben werden darf, ist
schreibgeschützt. Auswerfen nimmt eine Diskette heraus, und Diskette sichern
unter legt überall eine Kopie ab. Ein bespieltes Band wird gesichert, wenn es
herausgenommen wird. Schnappschuss sichern hält den ganzen Rechner in einer
Datei fest, und Schnappschuss wiederherstellen holt ihn samt Disketten und Band
zurück.

Das Menü, per Rechtsklick auf das Bild geöffnet, bietet jedes Laufwerk eines
Model B und das Band; BREAK, Zurücksetzen, Pause und die Geschwindigkeit;
Vollbild, scharfe oder weiche Skalierung und Zeilenlinien; die Belegung der
Tastatur; Einfügen, das den Text der Zwischenablage tippt; die Schnappschüsse;
Einstellungen; Rechnerbibliothek; und Herunterfahren. Nach Zeichen tippen die
Tasten, was sie zeigen, wobei die CAPS LOCK des Rechners die Groß- und
Kleinschreibung eines Buchstabens bestimmt. Nach Position ist jede Taste die
Taste des Rechners an ihrem Platz. Beim Model B sind F1 bis F9 f1 bis f9, F10
ist f0, Pos1 ist SHIFT LOCK und Einfg ist die Backslash-Taste. Beim Electron
sind F1 bis F10 FUNC mit 1 bis 9 und 0, Alt ist FUNC und die Feststelltaste ist
CAPS LK. Bei beiden ist F12 BREAK, Ende ist COPY und die Pfeiltasten sind die
Cursortasten.

Lampen am unteren Rand des Fensters eines Rechners zeigen CAPS LOCK und den
Kassettenmotor, beim Model B außerdem SHIFT LOCK und jedes Laufwerk. Jeder
Rechner läuft in einem eigenen Arbeitsprozess ohne jede Befugnis, sodass weder
seiner Software noch seinen Medien vertraut wird; ein Arbeitsprozess, der
versagt, wird ersetzt, der Rechner startet neu wie nach dem Einschalten, und
der Grund wird angezeigt.

Das Schließen des Bibliotheksfensters lässt laufende Rechner weiterlaufen, und
das Schließen aller Fenster lässt den Emulator in der Symbolleiste: Ein Klick
auf sein Symbol oder Rechnerbibliothek in seinem Menü holt die Bibliothek
zurück, und Beenden in diesem Menü versetzt jeden laufenden Rechner in den
Ruhezustand und beendet den Emulator.

## OPTIONS

`-h`, `-?`, `--help`
: Schreibt diese Hilfe auf die Standardausgabe und beendet sich.

## EXIT STATUS

Null, sobald im Menü seines Symbols in der Symbolleiste Beenden gewählt wird.
Ungleich null, wenn der Fensterkanal, das Ereignispostfach oder die
Desktop-Sitzung verweigert wurde; der Grund steht auf der
Standardfehlerausgabe.
