## NAME

TAIRiXBridge — TAIRiX Bridge, RISC-OS-Programme auf dem TAIRiX-Desktop

## SYNOPSIS

`TAIRiXBridge`

## DESCRIPTION

Führt RISC-OS-Programme — BASIC-Programme, Befehlszeilenwerkzeuge und
Einzelaufgaben-Spiele — auf dem TAIRiX-Desktop aus, jedes mit seinem
Bildschirm in einem eigenen Fenster. Das RISC OS, auf dem sie laufen, ist
TAIRiX Bridges eigenes, aus veröffentlichter Dokumentation geschrieben; es
enthält keinen RISC-OS-Code.

Programme laufen in Sitzungen. Die gemeinsame Sitzung ist der RISC-OS-Rechner,
den das Fenster TAIRiX Bridge zeigt: Ihr eigener Datenträger ist der Ordner
RISC OS in Ihren Dokumenten, ihre Startsequenz läuft beim Start, und ein
Programm aus Ihrem eigenen Speicher läuft in ihr. Ein Programm von anderswo —
Wechselmedien, ein Diskettenabbild, eine Netzwerkfreigabe — oder unbekannter
Herkunft läuft in einer eigenen, isolierten Sitzung, die Ihren Ordner RISC OS
nie sieht und mit ihrem letzten Programm endet. In gemeinsamer Sitzung
ausführen führt eine Datei in der gemeinsamen Sitzung aus, woher sie auch
stammt.

Das Fenster einer Sitzung listet ihre Aufgaben, jede mit ihrem Speicher und
ob sie angehalten oder beschäftigt ist, und was die gemeinsamen Bereiche der Sitzung belegen.
Ausführen wählt eine Datei zum Ausführen; eine auf das Fenster gezogene oder
mit TAIRiX Bridge geöffnete Datei läuft ebenfalls. Eine Aufgabe wählen und
dann Anhalten oder Beenden hält sie an oder beendet sie.

Der Bildschirm jedes Programms ist ein eigenes Fenster, sein Bild in den
Proportionen seines Modus auf das Fenster skaliert. Die Schaltfläche
Bildschirm öffnet das Menü des Fensters: Vollbild, scharfe oder weiche
Skalierung, Anhalten und Zurücksetzen, das das Programm beendet und es im
selben Fenster erneut ausführt. Der Bildschirm nimmt die ganze Tastatur, jede
Taste mit der Nummer, die RISC OS ihr gibt. Select, Menu und Adjust sind die
primäre, die sekundäre und die mittlere Taste. Sobald ein Programm die Maus
selbst bewegt, fängt ein Klick die Maus für es ein, und ein einzelnes Antippen
der Meta-Taste gibt sie frei.

Einstellungen wählt die Reihenfolge der Tasten, den Speicher, den jede Sitzung
nehmen darf, ob jedes Programm isoliert wird, und wie Bildschirme skaliert
werden. Der Arbeitsprozess einer Sitzung besitzt keine Befugnisse: Die Dateien,
die er liest und schreibt, werden für ihn übertragen, und ein Arbeitsprozess,
der versagt, beendet seine Sitzung; der Grund steht im Fenster der Sitzung.

## OPTIONS

`-h`, `-?`, `--help`
: Diese Hilfe auf die Standardausgabe schreiben und beenden.

## EXIT STATUS

Null, sobald das Fenster der letzten Sitzung geschlossen ist. Ungleich null,
wenn der Fensterkanal, das Ereignispostfach oder die Desktop-Sitzung verweigert
wurde; der Grund wird auf dem Standardfehlerstrom genannt.
