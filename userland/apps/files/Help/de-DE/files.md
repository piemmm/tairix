## NAME

files — grafischer Dateisystem-Browser

## SYNOPSIS

`files [--desktop] [verzeichnis] [-h | -?]`

## DESCRIPTION

Öffnet ein Desktop-Fenster, das das Dateisystem auflistet, beginnend mit dem
auf der Befehlszeile genannten `verzeichnis` oder, wenn keines genannt ist,
mit dem Ordner `UserFiles` des startenden Benutzers (seinem
Heimatverzeichnis, wenn dieser nicht aufgelistet werden kann). Der
Fenstertitel nennt das aktuelle Verzeichnis; das Fenster listet seine
Einträge, jeder ausgewählte Eintrag mit der Akzentfarbe des aktiven Themas
hervorgehoben. Jedes Lesen eines Verzeichnisses ist eine gewöhnliche,
berechtigungsgeprüfte Auflistung unter der Identität des startenden
Benutzers: ein nicht lesbares Verzeichnis wird abgelehnt, niemals erraten.

Der Desktop startet den Browser für Sie und hält ihn auf der Symbolleiste:
das Menü seines Platzes listet Ihre eigenen Orte und alles Eingebundene, und
die Wahl eines Eintrags öffnet dort ein Fenster. Ein Klick auf den Platz
öffnet eines im Ordner `UserFiles`. Wer einen Ordner verlangt, für den schon
ein Fenster offen ist, holt dieses Fenster nach vorn, statt ein weiteres zu
öffnen. Diese Instanz hat keine Zeile *Beenden* — sie ist Teil des Desktops,
und das Schließen ihrer Fenster räumt sie einfach weg.

Namentlich aus einer Shell gestartet (oder vom Desktop aus auf einem Ordner
geöffnet) ist er stattdessen eine gewöhnliche Anwendung: ein Fenster, und
sie endet, wenn Sie es schließen. In beiden Fällen benötigt er eine laufende
grafische Sitzung: ohne sie ist der Fensterkanal unerreichbar, und der
Browser meldet die Ablehnung auf dem Standardfehlerstrom und beendet sich.

Das Fenster wird mit der Tastatur bedient: `Runter` und `Hoch` bewegen die
Auswahl, `Eingabe` öffnet das ausgewählte Verzeichnis, und `Rücktaste`
wechselt in das übergeordnete Verzeichnis. `F5` liest die Auflistung und die
Orte-Leiste neu ein; ein neu angeschlossener Datenträger erscheint von
selbst in der Leiste. `Ctrl+Shift+N` legt einen neuen Ordner an.

Eine Auflistung öffnet sich ohne Auswahl. Ein Klick wählt einen Eintrag aus,
ein Klick mit `Ctrl` fügt einen hinzu oder nimmt ihn heraus, und ein Klick
mit `Shift` wählt die Folge ab dem zuletzt gewählten Eintrag; ein Klick auf
eine leere Fläche hebt die Auswahl auf. Ziehen über eine leere Fläche zieht
einen Rahmen auf, der beim Wachsen alles auswählt, was er berührt; am oberen
oder unteren Rand der Auflistung gehalten, rollt sie weiter, und `Escape`
nimmt zurück, was er ausgewählt hat.

Ausgewählte Einträge, die auf ein anderes Dateimanager-Fenster, einen Ordner
darin oder den Desktop gezogen werden, werden dorthin kopiert; mit
gehaltener Umschalttaste werden sie stattdessen verschoben. Der Zeiger zeigt
ein Plus, solange ein Ablegen kopieren würde, und einen Pfeil, solange es
verschieben würde, und der Ordner, in dem ein Ablegen landen würde, ist
hervorgehoben. Eine einzelne Datei, die auf den Platz einer Anwendung auf
der Symbolleiste gezogen wird, wird dort geöffnet.

Das Untermenü *Neu* des Rechtsklickmenüs legt einen Ordner oder ein leeres
Dokument jeder Art an, die ein installierter Editor schreibt, und öffnet
seinen Namen zum Bearbeiten.

`Alt+Enter` öffnet ein Fenster *Eigenschaften* für den ausgewählten Eintrag,
ebenso die Zeile *Eigenschaften* des Rechtsklickmenüs. Es ist ein eigenes
Fenster, sodass mehrere zugleich offen sein können und die Auflistung
währenddessen benutzbar bleibt: es zeigt, was der Eintrag ist, seine Größe,
seine Zeitstempel, wohin ein Alias zeigt, seine Berechtigungen und seinen
Eigentümer sowie die erweiterten Attribute, die der Datenträger dazu
speichert. Berechtigungen, Eigentümer und Attribute lassen sich dort ändern,
jeweils als gewöhnlicher, berechtigungsgeprüfter Schreibvorgang unter Ihrer
eigenen Identität — eine Ablehnung nennt den Grund und ändert nichts. Einen
Eigentümer neu zuzuweisen erfordert die Berechtigung `CAP_FS_CHOWN`; eine
Sitzung ohne sie sieht Eigentümer und Gruppe mit einem Schloss markiert und
eine Zeile, die den Grund nennt.

`Links` und `Rechts` wechseln zwischen den Abschnitten des Fensters. Unter
*Berechtigungen* führt `Runter` oder `Tab` in die Bedienelemente: die
Pfeiltasten wechseln zwischen ihnen, `Space` schaltet eine Berechtigung um
oder öffnet Eigentümer oder Gruppe zum Bearbeiten, und `Tab` oder `Escape`
kehrt zu den Abschnitten zurück.

Der Operand `verzeichnis` wird als nicht vertrauenswürdige Eingabe
behandelt: er muss ein absoluter Pfad innerhalb der Pfadlängengrenze
des Systems sein, und jeder seiner Bestandteile muss ein echter
Verzeichnisname sein — `.` und `..` sind das nicht, sodass eine
Schreibweise nie etwas anderes bedeuten kann, als sie zu lesen gibt.
Ein Verzeichnis, das eine dieser Regeln verletzt oder das der startende
Benutzer nicht auflisten darf, wird mit dem Grund auf dem
Standardfehlerstrom abgelehnt, und das Fenster öffnet stattdessen den
Ordner `UserFiles`, sodass ein falsches Argument den Benutzer nie ohne
Fenster lässt. Ein zweiter Operand wird rundweg abgelehnt statt
ignoriert.

## OPTIONS

- `--desktop` — als eigene Dateimanager-Komponente des Desktops laufen: ein
  dauerhafter Platz auf der Symbolleiste, der Ihre Orte und die eingebundenen
  Datenträger anbietet, kein Fenster, bis eines verlangt wird, und keine
  Möglichkeit zu beenden. Die Desktop-Sitzung übergibt dies beim Start; ein
  zusätzlich genanntes `verzeichnis` wird abgewiesen, denn eine Komponente öffnet
  kein Fenster, in das es gehören könnte.
- `-h, -?` — die kurze Hilfe dieses Befehls anzeigen und beenden.

## EXIT STATUS

Null nach sauberem Schließen oder nachdem die kurze Hilfe angezeigt
wurde; `2`, wenn die Befehlszeile nicht verstanden wurde; sonst
ungleich null, wenn der Fensterkanal, die gemeinsame Frame-Region oder
die erste Verzeichnisauflistung abgelehnt wurde (der Grund wird auf dem
Standardfehlerstrom genannt).
