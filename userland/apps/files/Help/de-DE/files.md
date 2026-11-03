## NAME

files — grafischer Dateisystem-Browser

## SYNOPSIS

`files --desktop [--mount] [pfad] [-h | -?]`

## DESCRIPTION

Der eigene Dateimanager des Desktops, der das Dateisystem in Fenstern
auflistet. Ein Fenster öffnet sich in dem Ordner, der verlangt wird — oder,
bei einer Datei, in dem Ordner, der diese Datei enthält, wobei die Datei
ausgewählt ist — oder, wenn keiner genannt ist, in dem Ordner, den die
Einstellungen wählen: im Ordner `Home` des startenden Benutzers, sofern er
nicht seinen Ordner `Desktop` oder die angeschlossenen Datenträger wählt
(in seinem Heimatverzeichnis, wenn dieser nicht aufgelistet werden kann). Der
Fenstertitel nennt das aktuelle Verzeichnis; das Fenster listet
seine Einträge, jeder ausgewählte Eintrag mit der Akzentfarbe des aktiven
Themas hervorgehoben. Jedes Lesen eines Verzeichnisses ist eine gewöhnliche,
berechtigungsgeprüfte Auflistung unter der Identität des startenden
Benutzers: ein nicht lesbares Verzeichnis wird abgelehnt, niemals erraten.

Der Desktop startet den Dateimanager mit der Sitzung und hält ihn auf der
Symbolleiste, und dieser beendet sich nie: das Menü seines Platzes listet
Ihre eigenen Orte und alles Eingebundene, und die Wahl eines Eintrags öffnet
dort ein Fenster; *Netzwerk* und *Einstellungen…* stehen zuletzt. Ein Klick
auf den Platz öffnet eines dort, wo die Einstellungen es bestimmen. Wer einen
Ordner verlangt, für den schon ein Fenster offen ist, holt dieses Fenster
nach vorn, statt ein weiteres zu öffnen. Das Menü hat keine Zeile *Beenden* —
der Dateimanager ist Teil des Desktops, das Schließen seiner Fenster räumt
ihn einfach weg, und hört er je auf zu laufen, startet ihn der Desktop neu.

*Netzwerk* im Menü des Platzes, direkt über *Einstellungen…*, öffnet ein
Fenster mit den Computern im lokalen Netzwerk, die Dateien freigeben. Jeder
wird mit seinem Namen, der Adresse, unter der er erreichbar ist, seiner
Arbeitsgruppe oder Domäne und seiner Art aufgeführt, und die Liste füllt
sich, während Computer antworten. Das Netzwerk wird nur durchsucht, solange
ein Fenster es zeigt. Ein Computer, der nicht mehr antwortet, wird
ausgegraut, bis er vergessen ist; ein Name, der Alphabete so mischt, wie es
die Nachahmung eines anderen Namens braucht, trägt statt seines Bildes ein
Warnzeichen; und bietet ein Computer mehr als einige wenige Einträge an,
werden die übrigen in einer eigenen Zeile gezählt, statt die anderen zu
verdrängen. Namen im Netzwerk wählt, wer den jeweiligen Computer betreibt,
daher beweist ein Name nie, welchen Computer Sie vor sich haben. Die
Verbindung zu den Freigaben eines Computers ist auf diesem System nicht
verfügbar, und das Öffnen eines Computers sagt dies.

Er läuft nur so, wie der Desktop ihn startet: auf andere Weise gestartet —
aus einer Shell über seinen Pfad `/System/Components/files.app` — lehnt er
den Start ab und nennt den Grund auf dem Standardfehlerstrom. Er benötigt
eine laufende grafische Sitzung: ohne sie ist der Fensterkanal unerreichbar,
und er meldet die Ablehnung auf dem Standardfehlerstrom und beendet sich.

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
Kopieren oder Verschieben behält die Lücken jeder Datei und ihre
erweiterten Attribute, etwa einen RISC-OS-Dateityp; ein Attribut, das das
Ziel nicht aufnehmen kann, wird nach Abschluss des Vorgangs in einer
Warnung genannt.

Das Rechtsklickmenü beginnt mit *Anzeige*. Darunter nennt ein Untermenü,
worauf das Menü wirkt — *Datei ‚notes.txt‘*, ein langer Name in der Mitte
gekürzt, sodass seine Endung sichtbar bleibt, oder *Auswahl*, wenn mehrere
Einträge ausgewählt sind — und enthält *Öffnen*, *Öffnen mit…*,
*Schreibgeschützt einhängen*, *Auswerfen*, *Informationen…*, *Umbenennen*,
*Ausschneiden*, *Kopieren* und *Löschen*; eine Zeile, die dort nicht wirken
kann, ist ausgegraut und nennt den Grund. Danach folgen *Neu*, dann *Alles
auswählen* und *Auswahl aufheben* und zuletzt *Einfügen*. Ein Rechtsklick
auf eine freie Stelle hebt die Auswahl auf, sodass das Untermenü des
Eintrags ausgegraut ist.

Das Öffnen eines Datenträgerabbilds oder Archivs — einer `.iso`-, `.adf`-,
`.hdf`- oder `.zip`-Datei und dergleichen — hängt es für Sie unter `/Storage`
ein, beschreibbar, wo sowohl die Datei als auch ihr Format es zulassen, und
öffnet sein Fenster; *Schreibgeschützt einhängen* hängt es stattdessen
schreibgeschützt ein. Dasselbe geschieht, wenn Sie eines auf dem Desktop
öffnen. Sobald kein Fenster es mehr zeigt, wird es wieder ausgehängt, sodass
`/Storage` nur enthält, was in Gebrauch ist; wird es noch von etwas anderem
benutzt — etwa von einem Dokument daraus, das in einer anderen Anwendung
geöffnet ist —, bleibt es eingehängt, und das Aushängen wird erneut versucht,
sobald dieses es freigegeben haben könnte. Die Einstellungen können das, was
Sie öffnen, stattdessen eingehängt lassen, bis Sie es auswerfen oder Ihre
Anmeldesitzung endet. *Auswerfen* nimmt einen Datenträger heraus, den Sie
eingehängt haben, von seinem Symbol oder von seiner Zeile in der
Orte-Leiste aus, deren eigenes Rechtsklickmenü *Öffnen* und *Auswerfen*
enthält; Ihre Fenster darauf wechseln zuvor nach `/Storage`. Ein
Datenträger, der weggefallen ist — weil seine Platte entfernt wurde oder
sein Server verloren ging —, bevor alles gespeichert war, was Sie darauf
geschrieben haben, verschwindet aus der Orte-Leiste, behält aber sein
Symbol in `/Storage`, und was nicht gespeichert wurde, ist verloren;
*Auswerfen* von diesem Symbol aus fragt nach, bevor es ihn aufgibt.

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
öffnet die Einstellungen des Browsers in der Anwendung Einstellungen, unter
*Dateien*: wo sich ein neues Fenster öffnet, wie Fenster ihre Einträge zeigen
und ordnen und ob ein neues die Orte-Leiste und die Werkzeugleiste zeigt;
ob Ordner oben bleiben; welche Namensendungen und welche Einträge
ausgeblendet sind; ob ein Bild seinen eigenen Inhalt zeigt und ein Ordner,
was er enthält; ob vor dem Verschieben von Einträgen in den Papierkorb
nachgefragt wird; und ob ein Datenträgerabbild oder Archiv, das Sie geöffnet
haben, ausgehängt wird, sobald seine Fenster geschlossen sind. Eine Änderung
gilt sofort für alle Fenster und wird gespeichert; ein Fenster behält eine
Anzeige oder eine Reihenfolge, die Sie selbst darin gewählt haben.

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

- `--desktop` — als eigener Dateimanager des Desktops laufen: ein dauerhafter
  Platz auf der Symbolleiste, der Ihre Orte und die eingebundenen Datenträger
  anbietet, und keine Möglichkeit zu beenden. Die Desktop-Sitzung übergibt
  dies immer, wenn sie den Dateimanager startet, der nur damit läuft; ein
  zusätzlich genannter `pfad` öffnet dort ein Fenster.
- `--mount` — `pfad` als Datenträgerabbild oder Archiv öffnen, wie beim
  Öffnen in einem Fenster: es wird für Sie eingehängt und sein Fenster
  öffnet sich, und es wird wieder ausgehängt, sobald seine Fenster
  geschlossen sind, wie es die Einstellungen vorsehen. Die Option benötigt
  einen `pfad`.
- `-h, -?` — die kurze Hilfe dieses Befehls anzeigen und beenden.

## EXIT STATUS

Er beendet sich nicht, solange der Desktop läuft. Null, nachdem die kurze
Hilfe angezeigt wurde; `2`, wenn die Befehlszeile nicht verstanden wurde oder
kein `--desktop` nannte; sonst ungleich null, wenn der Fensterkanal oder die
gemeinsame Frame-Region abgelehnt wurde (der Grund wird auf dem
Standardfehlerstrom genannt).
