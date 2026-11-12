## NAME

PCEmulator — Emulator für IBM-PC- und XT-Rechner

## SYNOPSIS

`PCEmulator`

## DESCRIPTION

Führt PCs aus der Zeit des 8088 bis 80186 in Desktop-Fenstern aus: von jedem
Rechner den Prozessor, die Grafikkarte, die Laufwerke, den Ton und die
Anschlüsse, in der Geschwindigkeit, die der Rechner hatte.

Das Bibliotheksfenster zeigt jeden Rechner als Kachel mit seinem letzten
Bildschirm. Neu legt einen Rechner aus einem von vier Modellen an — dem IBM
PC, dem IBM PC XT, einem Turbo-XT und einer 80186-Workstation — unter dem
gewünschten Namen und, bei einem Modell mit Festplattencontroller, mit einer
30-MB-Festplatte, die TAIRiX DOS startet. Starten oder ein Doppelklick startet
den gewählten Rechner in einem eigenen Fenster oder holt dessen Fenster nach
vorn; mehrere Rechner laufen zugleich. Einstellungen ändert die Hardware eines
Rechners, solange er aus ist, Duplizieren kopiert einen Rechner samt seinen
Platten, und Löschen löscht einen samt seinen Platten, sobald das bestätigt
ist. Ein Rechtsklick auf die Kachel eines Rechners bietet dies für diesen
Rechner an und, solange er läuft, auch das Menü des Rechners. Beim ersten Start legt PCEmulator einen Turbo-XT an. Was ein Rechner auf
seine Platten schreibt und die Zeit, auf die seine Uhr gestellt ist, bleiben
von einem Lauf zum nächsten erhalten.

Der Turbo-XT und die 80186-Workstation haben einen numerischen
8087-Koprozessor, und Einstellungen baut jedem Rechner einen ein oder nimmt
ihn heraus. Ein Programm, das auf den Koprozessor wartet, ohne sich zuvor zu
vergewissern, dass einer da ist, bleibt auf einem Rechner ohne ihn stehen, wie
es auf der echten Hardware geschah.

Das Bild ist das des Monitors, vier Einheiten breit auf drei hoch, wobei jeder
Punkt der Grafikkarte scharfkantig bleibt, wie groß das Fenster auch ist. Das
Schließen des Fensters eines Rechners versetzt den Rechner in den Ruhezustand:
wieder gestartet, macht er dort weiter, wo er stehen blieb, mit leeren
Diskettenlaufwerken und einer Uhr, die um die Zeit seiner Abwesenheit
vorgestellt ist. Die Hardware eines ruhenden Rechners wird geändert, nachdem
er gestartet und heruntergefahren wurde.

Am Fuß des Fensters eines Rechners hat jedes seiner Laufwerke eine Lampe,
die leuchtet, während das Laufwerk arbeitet. Ein Druck auf ein
Diskettenlaufwerk dort bietet Diskette einlegen an, das ein Diskettenabbild
für das Laufwerk auswählt, und Auswerfen. Ein Diskettenabbild, das auf das
Fenster gezogen wird, kommt in das Laufwerk, auf dem es abgelegt wird, sonst
in das erste, und kann dort nicht beschrieben werden.

Am Ende dieser Leiste öffnet die Schaltfläche Rechner das Menü des Rechners,
das den Rechner anhält, ihn zurücksetzt, ihm Strg+Alt+Entf sendet und die
Geschwindigkeit seines Prozessors für den Lauf einstellt. Anzeige schaltet Zeilenraster, Leuchten,
Nachleuchten, Rauschen und Wackeln des Bildes ein und aus, und der Rechner
behält, was gewählt ist. Bildschirmfoto aufnehmen sichert den Bildschirm als
PNG-Bild dort, wo das Sichern-Feld es angibt. Vollbild füllt den Bildschirm
mit dem Fenster und lässt die Leiste stehen, wo dasselbe Menü den Bildschirm
wieder freigibt. Herunterfahren schaltet den Rechner aus und behält nur seinen
letzten Bildschirm.

Bildschirmtext kopieren legt den Text, den der Bildschirm des Rechners zeigt,
in die Zwischenablage, solange er Text zeigt. Einfügen tippt den Text der
Zwischenablage auf der Tastatur des Rechners, jedes Zeichen, sobald das
Programm das vorige gelesen hat. Was der Rechner nicht als es selbst tippen
kann, etwa die Gesichter und Pfeile des PCs oder einen Buchstaben, den er nicht
kennt, wird ausgelassen, und das Fenster sagt es. Einfügen beenden oder eine
Taste, die im Fenster des Rechners gedrückt wird, beendet das Tippen.

Ordner freigeben leiht dem Rechner einen Ihrer Ordner, in der Auswahl gewählt,
als eigenes Laufwerk: den ersten Buchstaben, den seine Platten frei lassen,
sobald TAIRiX DOS gestartet ist. Einen Namen, den DOS nicht fassen kann, kürzt
es, wie Windows einen langen Namen kürzt, und ein nur zum Lesen freigegebener
Ordner oder eine Datei, die Sie nicht ändern dürfen, lässt sich vom Rechner
aus nicht ändern. Jeder freigegebene Ordner hat einen Platz in der Leiste, der
sein Laufwerk nennt; ein Druck darauf bietet Freigabe beenden an. Ein Rechner
gibt bis zu vier Ordner frei, und keinen mehr, sobald er im Ruhezustand ist
oder heruntergefahren wurde.

Debugger im Menü des Rechners öffnet ein Fenster auf den Rechner: seine
Register, seinen 8087, seinen Code und Speicher, wie sie beim Anhalten
standen, und seine Haltepunkte. Unterbrechen hält den Rechner an, wo er gerade
ist; Fortsetzen, Schritt, Prozedurschritt und Rücksprung lassen ihn
weiterlaufen, wie es F5, F11, F10 und Umschalt+F11 tun, wobei F5 einen
laufenden Rechner auch anhält. Ein Druck neben eine Codezeile setzt dort einen
Haltepunkt oder entfernt ihn, wie es F9 für die aktuelle Zeile tut, und
Haltepunkt hinzufügen hält den Rechner stattdessen an, wenn er Speicher
schreibt oder liest, einen Anschluss liest oder schreibt oder in einen
Interrupt eintritt. Die Zeit des Rechners steht still, solange er angehalten
ist, und das Schließen des Debuggers lässt ihn weiterlaufen.

Solange das Fenster eines Rechners die Tastatur hat, geht jede Taste als
Taste seiner eigenen Tastatur an den Rechner, Strg, Alt und die
Feststelltasten eingeschlossen; der Desktop behält Strg+Alt+Entf, seine
Aufnahmekürzel und die Meta-Taste. Ein Programm, das die Maus über den
DOS-Maustreiber nutzt, folgt dem Desktop-Zeiger, wo immer er über dem Bild
steht. Ein Programm, das die Maus selbst liest, übernimmt sie, wenn man auf
das Bild klickt, und gibt sie zurück, wenn man kurz die Meta-Taste antippt.

Das Schließen des Bibliotheksfensters lässt laufende Rechner weiterlaufen, und
das Schließen aller Fenster lässt den Emulator in der Symbolleiste: Ein Klick
auf sein Symbol oder Rechnerbibliothek in seinem Menü holt die Bibliothek
zurück, und Beenden in diesem Menü versetzt jeden laufenden Rechner in den
Ruhezustand und beendet den Emulator. Jeder Rechner
läuft in einem eigenen Arbeitsprozess, der keinerlei Befugnisse hat: seiner
Software und seinen Platten wird nie vertraut.

## OPTIONS

`-h`, `-?`, `--help`
: Diese Hilfe auf die Standardausgabe schreiben und beenden.

## EXIT STATUS

Null, sobald Beenden in seinem Menü in der Symbolleiste gewählt wurde. Ungleich
null, wenn der Fensterkanal, das Ereignispostfach oder die Desktop-Sitzung
verweigert wurde; der Grund wird auf der Standardfehlerausgabe genannt.
