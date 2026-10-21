## NAME

files — grafischer Dateisystem-Browser

## SYNOPSIS

`files [--desktop] [pfad] [-h | -?]`

## DESCRIPTION

Öffnet ein Desktop-Fenster, das das Dateisystem auflistet, beginnend mit dem
Ordner, den `pfad` auf der Befehlszeile nennt — oder, wenn er eine Datei
nennt, mit dem Ordner, der diese Datei enthält, wobei die Datei ausgewählt
ist — oder, wenn keiner genannt ist, mit dem Ordner, den die Einstellungen
wählen: dem Ordner `Home` des startenden Benutzers, sofern er nicht seinen
Ordner `Desktop` oder die angeschlossenen Datenträger wählt (seinem
Heimatverzeichnis, wenn dieser nicht aufgelistet werden kann). Der
Fenstertitel nennt das aktuelle Verzeichnis; das Fenster listet
seine Einträge, jeder ausgewählte Eintrag mit der Akzentfarbe des aktiven
Themas hervorgehoben. Jedes Lesen eines Verzeichnisses ist eine gewöhnliche,
berechtigungsgeprüfte Auflistung unter der Identität des startenden
Benutzers: ein nicht lesbares Verzeichnis wird abgelehnt, niemals erraten.

Der Desktop startet den Browser für Sie und hält ihn auf der Symbolleiste:
das Menü seines Platzes listet Ihre eigenen Orte und alles Eingebundene, und
die Wahl eines Eintrags öffnet dort ein Fenster; *Einstellungen…* steht
zuletzt. Ein Klick auf den Platz öffnet eines dort, wo die Einstellungen es
bestimmen. Wer einen Ordner verlangt, für den schon ein Fenster offen ist,
holt dieses Fenster nach vorn, statt ein weiteres zu öffnen. Diese Instanz
hat keine Zeile *Beenden* — sie ist Teil des Desktops, und das Schließen
ihrer Fenster räumt sie einfach weg.

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
eine leere Fläche hebt die Auswahl auf. Ein Doppelklick öffnet einen
Eintrag; mit gehaltener `Shift`-Taste listet er den Inhalt einer Anwendung
auf, statt sie zu starten, und was er anderswo öffnet, bleibt nicht
ausgewählt. Ziehen über eine leere Fläche zieht einen Rahmen auf, der beim
Wachsen alles auswählt, was er berührt; am oberen oder unteren Rand der
Auflistung gehalten, rollt sie weiter, und `Escape` nimmt zurück, was er
ausgewählt hat.

Der Raum um jedes Symbol und seinen Namen gilt als leere Fläche, sodass ein
Rahmen überall zwischen den Einträgen beginnen kann. `Ctrl+A` wählt alles in
der Auflistung aus, und `Ctrl+Shift+A` hebt die Auswahl auf; beides steht
auch im Rechtsklickmenü.

Der Name eines Eintrags wird vollständig gezeigt, bei Bedarf auf zwei
Zeilen; ein Name, der selbst dafür zu lang ist, behält seinen Anfang und
sein Ende mit `…` in der Mitte, sodass seine Endung immer sichtbar bleibt.
Eine Anwendung erscheint ohne ihre Endung `.app`; die Einstellungen können
diese zeigen oder auch die Endungen der Dateitypen ausblenden, die das
System kennt. Einträge, deren Namen mit einem Punkt beginnen, werden nur
aufgeführt, wenn die Einstellungen es verlangen.

`F2` benennt den ausgewählten Eintrag an Ort und Stelle um, ebenso ein Klick
auf den Namen des einzigen ausgewählten Eintrags mit einer kurzen Pause
danach: Der Name öffnet sich zum Bearbeiten, sobald der Klick kein
Doppelklick mehr werden kann. Der Teil vor der Endung ist ausgewählt, sodass
Tippen den Namen ersetzt und die Endung behält; eine Endung, die das Fenster
nicht zeigt, bleibt ebenfalls erhalten. `Eingabe` oder ein Klick außerhalb
des Namens übernimmt den neuen Namen — lehnt der Datenträger ihn ab, bleibt
das Feld mit dem Grund geöffnet —, und `Escape` verwirft ihn. Ein Klick, der
den Eintrag umbenannt hat, bewirkt sonst nichts, da sich der Ordner unter
dem Zeiger neu sortiert haben kann.

Ausgewählte Einträge, die auf ein anderes Dateimanager-Fenster, einen Ordner
darin oder den Desktop gezogen werden, werden dorthin kopiert; mit
gehaltener Umschalttaste werden sie stattdessen verschoben. Der Zeiger zeigt
ein Plus, solange ein Ablegen kopieren würde, und einen Pfeil, solange es
verschieben würde, und der Ordner, in dem ein Ablegen landen würde, ist
hervorgehoben. Eine einzelne Datei, die auf den Platz einer Anwendung auf
der Symbolleiste gezogen wird, wird dort geöffnet.

Das Rechtsklickmenü beginnt mit *Anzeige*. Darunter nennt ein Untermenü,
worauf das Menü wirkt — *Datei ‚notes.txt‘*, ein langer Name in der Mitte
gekürzt, sodass seine Endung sichtbar bleibt, oder *Auswahl*, wenn mehrere
Einträge ausgewählt sind — und enthält *Öffnen*, *Öffnen mit…*,
*Informationen…*, *Umbenennen*, *Ausschneiden*, *Kopieren* und *Löschen*;
eine Zeile, die dort nicht wirken kann, ist ausgegraut und nennt den Grund.
Danach folgen *Neu*, dann *Alles auswählen* und *Auswahl aufheben* und
zuletzt *Einfügen*. Ein Rechtsklick auf eine freie Stelle hebt die Auswahl
auf, sodass das Untermenü des Eintrags ausgegraut ist.

Das Untermenü *Neu* des Rechtsklickmenüs legt einen Ordner oder ein leeres
Dokument jeder Art an, die ein installierter Editor schreibt, und öffnet
seinen Namen zum Bearbeiten.

Das Untermenü *Anzeige* des Rechtsklickmenüs zeigt die Einträge als große
Symbole, die Voreinstellung; als kleine Symbole, jeder Name in einer Zeile
neben seinem Bild; oder als Liste, eine Zeile je Eintrag mit seinem Bild,
seiner Größe, dem Zeitpunkt der letzten Änderung und seinem Typ. Ein darin
aufgeführtes Laufwerk zeigt stattdessen seinen freien Speicher, sein
Dateisystem und wie gut es arbeitet. Unter den Anzeigearten ordnet es das
Fenster nach Name, Typ, Größe oder dem Zeitpunkt der letzten Änderung jedes
Eintrags — oder, bei einer Liste von Laufwerken, nach Name, freiem Speicher,
Dateisystem oder Zustand — auf- oder absteigend. Ein Klick auf den Titel
einer Spalte ordnet die Liste ebenfalls danach, ein zweiter Klick kehrt die
Reihenfolge um. Ordner bleiben bei jeder gewählten Reihenfolge oben, sofern
die Einstellungen sie nicht zwischen die Dateien stellen.

*Einstellungen…* im Menü des Platzes auf der Symbolleiste oder `Ctrl+,`
öffnet die Einstellungen des Browsers: wo sich ein neues Fenster öffnet, wie
es seine Einträge zeigt und ordnet und ob es die Orte-Leiste und die
Werkzeugleiste zeigt; ob Ordner oben bleiben; welche Namensendungen und
welche Einträge ausgeblendet sind; ob ein Bild seinen eigenen Inhalt zeigt
und ein Ordner, was er enthält; und ob vor dem Verschieben von Einträgen in
den Papierkorb nachgefragt wird. *Standardwerte wiederherstellen* setzt jede
Einstellung zurück. Eine Änderung gilt sofort für alle Fenster und wird
gespeichert; ein bereits geöffnetes Fenster behält seine Anzeige und
Reihenfolge.

`Alt+Enter` öffnet das Fenster *Informationen* des ausgewählten Eintrags,
ebenso *Informationen…* in dessen Untermenü des Rechtsklickmenüs. Es ist ein eigenes
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

Der Operand `pfad` wird als nicht vertrauenswürdige Eingabe behandelt:
er muss ein absoluter Pfad innerhalb der Pfadlängengrenze des Systems
sein, und jeder seiner Bestandteile muss ein echter Name sein — `.` und
`..` sind das nicht, sodass eine Schreibweise nie etwas anderes bedeuten
kann, als sie zu lesen gibt. Ein Pfad, der eine dieser Regeln verletzt
oder den der startende Benutzer nicht auflisten darf, wird mit dem Grund
auf dem Standardfehlerstrom abgelehnt, und das Fenster öffnet stattdessen
den Ordner `Home`, sodass ein falsches Argument den Benutzer nie ohne
Fenster lässt. Ein zweiter Operand wird rundweg abgelehnt statt ignoriert.

## OPTIONS

- `--desktop` — als eigene Dateimanager-Komponente des Desktops laufen: ein
  dauerhafter Platz auf der Symbolleiste, der Ihre Orte und die eingebundenen
  Datenträger anbietet, kein Fenster, bis eines verlangt wird, und keine
  Möglichkeit zu beenden. Die Desktop-Sitzung übergibt dies beim Start; ein
  zusätzlich genannter `pfad` wird abgewiesen, denn eine Komponente öffnet
  kein Fenster, in das er gehören könnte.
- `-h, -?` — die kurze Hilfe dieses Befehls anzeigen und beenden.

## EXIT STATUS

Null nach sauberem Schließen oder nachdem die kurze Hilfe angezeigt
wurde; `2`, wenn die Befehlszeile nicht verstanden wurde; sonst
ungleich null, wenn der Fensterkanal, die gemeinsame Frame-Region oder
die erste Verzeichnisauflistung abgelehnt wurde (der Grund wird auf dem
Standardfehlerstrom genannt).
