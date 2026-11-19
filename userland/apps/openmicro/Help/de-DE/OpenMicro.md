## NAME

OpenMicro — Open Micro Emulator, Emulator für 8-Bit-Mikrocomputer

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Führt 8-Bit-Mikrocomputer in Desktop-Fenstern aus — den BBC Micro Model B, den
Acorn Electron und den Commodore 64 —, Takt für Takt in ihrer eigenen
Geschwindigkeit, jeden mit seinem eigenen, im Reinraumverfahren geschriebenen
Betriebssystem und BASIC ausgestattet, den Model B zudem mit einem
Diskettendateisystem und den Commodore 64 mit einem Diskettenlaufwerk 1541 samt
eigenem DOS, sodass ein Rechner an der BASIC-Eingabe startet. Das Bild ist das
eines Fernsehers, vier Einheiten breit auf drei hoch, und der Ton erklingt über
das Audio des Desktops.

Das Bibliotheksfenster zeigt jeden Rechner als Kachel mit seinem letzten
Bildschirm oder, bis er gelaufen ist, mit dem Startbildschirm seines Modells.
Neu legt einen Rechner unter dem gewünschten Namen an: einen BBC Micro Model
B, mit Seiten-RAM in den Steckplätzen 4 bis 7 und einer neuen, leeren Diskette
in Laufwerk 0 oder ohne; einen Acorn Electron, mit angebautem Plus 1 oder
ohne; oder einen Commodore 64, PAL oder NTSC, mit einem SID 6581 oder 8580 und
einem Laufwerk 1541, zwei oder keinem. Starten, ein Doppelklick oder die
Eingabetaste startet den gewählten Rechner in einem eigenen Fenster oder holt
dessen Fenster nach vorn; bis zu zwanzig Rechner werden aufbewahrt, und
mehrere laufen zugleich. Duplizieren kopiert einen Rechner samt seinem
gesicherten Zustand und seinem Ordner, und Löschen löscht einen Rechner und
seinen gesicherten Zustand, sobald das bestätigt ist, lässt seinen Ordner aber
bestehen. Ein Rechtsklick auf die Kachel eines Rechners bietet Starten,
Einstellungen, Duplizieren, Löschen und Ordner anzeigen für diesen Rechner an
und, solange er läuft, auch das Menü des Rechners. Beim ersten Start legt der
Emulator einen BBC Micro Model B mit einer leeren Diskette an.

Einstellungen ändert den Namen eines Rechners, seine Lautstärke, das schnelle
Laden, das den Rechner so schnell wie möglich laufen lässt, solange der
Kassettenmotor läuft oder sich in einem Laufwerk eine Diskette dreht und der
Rechner still ist, und beim Commodore 64 den Control-Port, in dem sein
Ziffernblock-Joystick steckt. Was in ihn eingebaut ist, lässt sich nur ändern,
solange er heruntergefahren ist: das ROM in jedem Sockel — OS und sechzehn
Seitensteckplätze beim Model B, OS und BASIC beim Electron, ROM und Steckmodule
bei seinem Plus 1 sowie KERNAL, BASIC, Zeichensatz und das DOS jedes Laufwerks
beim Commodore 64 —, jeweils ein mitgeliefertes ROM, Seiten-RAM in einem
Steckplatz des Model B, nichts in einem Steckplatz oder Steckmodul oder ein
eigenes ROM-Abbild, das mit Abbild installieren im Speicher des Emulators
abgelegt wurde; das Plus 1 des Electron; Fernsehnorm, SID und Laufwerke des
Commodore 64; und die Tastaturbrücken des Model B, die seinen Bildschirmmodus
beim Start, das Starten von Diskette und die Diskettenzeiten festlegen. Einige
wenige Programme lesen Code aus den Original-ROMs und laufen nur, wenn Abbilder
dieser ROMs eingebaut sind.

Ein Model B hat außerdem VIM, einen Editor im Stil von vi, in Steckplatz 13
eingebaut. An der Eingabe des Rechners bearbeitet `*VIM` mit dem Namen einer
Datei diese Datei, und BREAK in VIM behält den Text, den der Editor enthält.
VIM bearbeitet Text, etwa die `!BOOT`-Dateien, die `*BUILD` anlegt, und
BASIC-Programme, die er als Text auflistet und tokenisiert wieder sichert, und
färbt beides ein, soweit die Farben des Bildschirmmodus es zulassen. In VIM
listet `:help` seine Befehle und die Bildschirmmodi samt dem jeweils im
Hauptspeicher verbleibenden Platz auf, `:mode` mit einer Zahl wechselt den
Modus, und `:q` kehrt zu BASIC zurück. Ist der Hauptspeicher voll, nutzt VIM
zusätzlich bis zu zwei Bänke Seiten-RAM, die kein ROM enthalten, sodass sich
auch eine Datei bearbeiten lässt, die größer als der Hauptspeicher ist; das
Lesen und Schreiben einer solchen Datei dauert länger.

Das Schließen des Fensters eines Rechners versetzt den Rechner in den
Ruhezustand: wieder gestartet, macht er dort weiter, wo er stehen blieb.
Herunterfahren in seinem Menü nimmt seine Medien heraus, schaltet ihn aus und
behält nur seinen letzten Bildschirm.

Jeder Rechner hat in Ihrem Ordner Documents einen eigenen Ordner: Open Micro
und darin der Name des Rechners. Neue Diskette, auch beim Commodore 64, und
Neues Band erstellen dort eine leere Diskette oder ein leeres Band und legen es
ein, Aus dem Ordner bietet die Disketten und Bänder an, die schon dort liegen,
und Schnappschuss sichern hält den ganzen Rechner dort als `.snapshot`-Datei
fest. Ordner anzeigen öffnet den Ordner in Files, sodass sich das, was der
Rechner dort aufbewahrt, auf andere Medien oder einen anderen Computer
kopieren lässt. Was seine Laufwerke und sein Kassettenrekorder aus seinem
Ordner enthalten, bleibt beim Rechner: heruntergefahren, legt er es beim Start
wieder ein; aus dem Ruhezustand fortgesetzt, schreibt er nur dorthin zurück,
wo seitdem kein anderes Programm etwas geändert hat.

Ein Diskettenabbild (`.ssd`, `.dsd`) oder Bandabbild (`.uef`, `.csw`) oder, für
einen Commodore 64, ein Diskettenabbild (`.d64`, `.g64`), Bandabbild (`.tap`),
Steckmodulabbild (`.crt`) oder Programm (`.prg`, `.t64`), das mit dem Emulator
geöffnet wird, geht an den laufenden Rechner, dessen Fenster zuletzt aktiv war,
oder an einen anderen laufenden Rechner, der es aufnimmt, oder sonst an den
ersten Rechner der Bibliothek, der es aufnimmt und dafür gestartet wird, und
das Fenster dieses Rechners kommt nach vorn. Das Öffnen einer
BBC-Micro-Diskette in Files zeigt die Dateien darauf, statt einen Rechner zu
starten. Eines, das mit Diskette einlegen oder Band einlegen gewählt wird,
kommt in das betreffende Laufwerk oder in den Kassettenrekorder, und eines,
das auf das Fenster eines Rechners gezogen wird, kommt schreibgeschützt in
sein erstes Laufwerk oder in den Rekorder, wobei das Fenster die Tastatur
übernimmt. Was der Rechner auf eine Diskette schreibt, geht in die Datei der
Diskette zurück, solange diese Datei beschrieben werden darf, und solange die
Diskette im Laufwerk liegt, darf weder ein eingehängter Datenträger noch ein
anderer Rechner die Datei beschreiben; eine Diskette, deren Datei nicht
beschrieben werden darf oder die ein anderes Programm benutzt, ist
schreibgeschützt, und das Fenster nennt den Grund. Für eine `.d64`-Diskette,
die inzwischen ein `.g64`-Abbild braucht, wird angeboten, sie als solches zu
sichern. Auswerfen nimmt eine Diskette heraus, und Diskette sichern unter legt
überall eine Kopie ab. Ein bespieltes Band wird gesichert, wenn es
herausgenommen wird. Beim Commodore 64 bedienen Abspielen, Aufnehmen,
Stoppen und Zurückspulen im Menü Band seine Datasette; Steckmodul einstecken
steckt ein Steckmodul ein und Steckmodul entfernen zieht es heraus, wobei
beide den Rechner neu starten; und Programm laden legt ein Programm in den
Speicher, wie es LOAD täte, damit RUN es startet. Schnappschuss sichern hält
den ganzen Rechner in einer Datei fest — in seinem Ordner oder, bevor er einen
hat, an einem Ort Ihrer Wahl —, und Schnappschuss wiederherstellen holt einen
so festgehaltenen Rechner samt seinen Medien zurück.

Das Menü, per Rechtsklick auf das Bild geöffnet, bietet jedes Laufwerk eines
Model B oder eines Commodore 64 und das Band sowie beim Commodore 64 das
Steckmodul und den Eintrag Programm laden; BREAK oder beim Commodore 64
RESTORE; beim Model B und beim Electron SHIFT-BREAK, das BREAK drückt, während
SHIFT gehalten wird, um von der Diskette oder vom Band zu starten;
Zurücksetzen, Pause und die Geschwindigkeit; Vollbild, scharfe oder
weiche Skalierung und Zeilenlinien; die Belegung der Tastatur; Einfügen, das
den Text der Zwischenablage tippt, jeden Buchstaben in der Groß- oder
Kleinschreibung, die der Rechner zeigt; die Schnappschüsse; Ordner anzeigen;
Einstellungen; Rechnerbibliothek; und Herunterfahren. Nach Zeichen tippen die
Tasten, was sie zeigen, wobei die CAPS LOCK des Rechners, beim Commodore 64
die Umschalttaste, die Groß- und Kleinschreibung eines Buchstabens bestimmt.
Nach Position ist jede Taste die Taste des Rechners an ihrem Platz. Beim Model
B sind F1 bis F9 f1 bis f9, F10 ist f0, Pos1 ist SHIFT LOCK und Einfg ist die
Backslash-Taste. Beim Electron sind F1 bis F10 FUNC mit 1 bis 9 und 0, Alt ist
FUNC und die Feststelltaste ist CAPS LK. Bei beiden ist F12 BREAK, Ende ist
COPY und die Pfeiltasten sind die Cursortasten. Beim Commodore 64 sind F1, F3,
F5 und F7 seine Funktionstasten und F2, F4, F6 und F8 dieselben mit SHIFT,
Escape ist RUN/STOP, Alt ist die Commodore-Taste, Strg ist CTRL, Pos1 ist
CLR/HOME, F12 oder Bild auf ist RESTORE, und die Pfeiltasten sind die
Cursortasten; bei eingeschaltetem Ziffernblock-Joystick bewegen die
Pfeiltasten des Ziffernblocks den Joystick, und 0 und 5 feuern.

Lampen am unteren Rand des Fensters eines Rechners zeigen CAPS LOCK und den
Kassettenmotor, beim Model B außerdem SHIFT LOCK und jedes Laufwerk; beim
Commodore 64 zeigen sie den Motor der Datasette und jedes Laufwerk. Beim Model
B und beim Electron tut die Schaltfläche SHIFT-BREAK rechts am unteren Rand,
was die Zeile im Menü tut. Jeder Rechner läuft in einem eigenen Arbeitsprozess
ohne jede Befugnis, sodass weder seiner Software noch seinen Medien vertraut
wird; ein Arbeitsprozess, der versagt, wird ersetzt, der Rechner startet neu
wie nach dem Einschalten, und der Grund wird angezeigt.

Das Schließen des Bibliotheksfensters lässt laufende Rechner weiterlaufen, und
das Schließen aller Fenster lässt den Emulator in der Symbolleiste: Ein Klick
auf sein Symbol oder Rechnerbibliothek in seinem Menü holt die Bibliothek
immer nach vorn, und Beenden in diesem Menü versetzt jeden laufenden Rechner
in den Ruhezustand und beendet den Emulator.

## OPTIONS

`-h`, `-?`, `--help`
: Schreibt diese Hilfe auf die Standardausgabe und beendet sich.

## EXIT STATUS

Null, sobald im Menü seines Symbols in der Symbolleiste Beenden gewählt wird.
Ungleich null, wenn der Fensterkanal, das Ereignispostfach oder die
Desktop-Sitzung verweigert wurde; der Grund steht auf der
Standardfehlerausgabe.
