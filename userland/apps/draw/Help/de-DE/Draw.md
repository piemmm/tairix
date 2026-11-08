## NAME

Draw — Editor für Vektorzeichnungen

## SYNOPSIS

`Draw`

## DESCRIPTION

Zeichnet und bearbeitet Vektorzeichnungen in einem Fenster des Desktops:
Pfade, Rechtecke, Ellipsen, Sterne und Vielecke, Linien, Freihandkurven und
Text, in Gruppen und Ebenen, auf einer Seite. Mit einem Dokument gestartet —
aus der Dateiverwaltung, vom Desktop oder indem eine Datei auf sein Symbol in
der Symbolleiste gezogen wird — öffnet es ein Fenster dafür. Allein gestartet,
öffnet es eine neue Zeichnung auf einer A4-Seite. Jedes Dokument ist ein
Fenster des einen Programms; das Schließen des letzten lässt es in der
Symbolleiste, und die Zeile Beenden seines Symbolmenüs beendet es.

Draw öffnet und speichert SVG und öffnet RISC-OS-Draw-Dateien, die es als SVG
speichert. Eine Zeichnung behält ihre Seite und Einheiten, ihre Ebenen,
Gruppen und Ausschnitte, jeden Pfad, jede Form, jeden Text und jedes Bild,
Vollfarben und lineare und radiale Verläufe, die Deckkraft jedes Objekts,
Strichelungen, Ecken, Enden und Pfeilspitzen, die Hilfslinien und das Raster
sowie den Titel der Zeichnung. Eine Datei, die enthält, was eine Zeichnung
nicht behalten kann — Filtereffekte, Masken, Musterfüllungen, Marker,
unlesbare Bilder, Textstile oder Textpfade, die ein Text nicht halten kann,
Animation oder Skripte unter anderem —, öffnet sich schreibgeschützt; das
Statusband nennt, was sie enthält, und Speichern fragt nach einem neuen Namen,
damit das Original nie überschrieben wird. Kopie speichern schreibt die
Zeichnung als PDF; als EPS, ihre Transparenz deckend gemacht, wie PostScript
sie malt; als PNG, JPEG oder TIFF, in der Kopierauflösung der Seite und bis zu
8192 Pixel an ihrer längeren Seite; als SVG; oder als RISC-OS-Draw-Datei und
nennt, was das Format von der Zeichnung nicht halten konnte, etwa Verläufe;
jede lässt die Datei des Dokuments, wie sie ist.

Der Werkzeugkasten läuft am linken Rand des Fensters hinab, der Inspektor am
rechten, und das Statusband läuft am Fuß entlang: Es zeigt die Stelle des
Zeigers und die Größe der Auswahl in den Einheiten der Seite und sagt, was der
letzte Befehl getan hat. Oben sagt die Leiste, wie das Werkzeug in der Hand
benutzt wird, und zeigt den Zoom: Ein Druck auf den Zoom öffnet ein Menü von
Zoomstufen. Lineale in den Einheiten der Seite säumen die Leinwand oben und
links, Bildlaufleisten rechts und unten. Verweilt der Zeiger auf der
Schaltfläche eines Werkzeugs, nennt sie es mit der Taste, die es wählt.

Die Werkzeuge und die Tasten, die sie wählen, sind: Auswahl (`V`), Knoten
(`A`), Rechteck (`R`), Ellipse (`E`), Stern und Vieleck (`S`), Linie (`L`),
Feder (`B`), Bleistift (`P`), Text (`T`), Verlauf (`G`), Pipette (`I`), Zoom
(`Z`) und Hand (`H`).

Das Auswahlwerkzeug nimmt mit einem Druck ein Objekt, eine Gruppe als Ganzes,
und Umschalt fügt der Auswahl ein Objekt hinzu oder nimmt es heraus. Ein Druck
auf leere Leinwand zieht einen Rahmen auf, der wählt, was ganz in ihm liegt,
oder mit Alt alles, was er berührt. Ziehen der Auswahl verschiebt sie, mit
Umschalt entlang einer Achse, und mit Alt verschiebt es eine Kopie. Ihre acht
Griffe skalieren sie vom gegenüberliegenden Griff aus, eine Ecke mit Umschalt
unter Wahrung der Proportionen; Ziehen knapp außerhalb einer Ecke dreht sie um
ihre Mitte, mit Umschalt in Schritten von 15 Grad. Die Pfeiltasten verschieben
sie um ein Pixel, mit Umschalt um zehn.

Das Knotenwerkzeug zeigt die Knoten des gewählten Pfads und die Griffe seiner
Kurven: Ziehen Sie einen Knoten, um ihn zu verschieben, oder einen Griff, um
seine Kurve zu biegen; der andere Griff eines glatten Knotens folgt in einer
Linie. Rechtecke, Ellipsen, Sterne und Linien werden durch Ziehen gezeichnet:
Umschalt macht die Seiten gleich — ein Quadrat, ein Kreis, eine Linie in einem
Vielfachen von 15 Grad —, und Alt zeichnet vom Anfangspunkt des Ziehens aus
nach außen. Ein Stern wird von seiner Mitte aus gezogen; seine Zacken, sein
Innenradius und ob er ein Vieleck ist, werden im Reiter Form des Inspektors
eingestellt, ebenso die Ecken eines neuen Rechtecks.

Die Feder setzt einen Pfad Knoten für Knoten: Klicken Sie für eine Ecke, oder
ziehen Sie für einen glatten Knoten und seine Kurve. Ein Klick auf den ersten
Knoten schließt den Pfad, und Eingabe oder die Wahl eines anderen Werkzeugs
beendet ihn offen; `Delete` oder `Backspace` nimmt den letzten Knoten zurück,
und Escape verwirft den Pfad. Der Bleistift zeichnet frei Hand, und seine
Linie wird beim Loslassen in glatte Kurven gefasst. Das Textwerkzeug beginnt
Text, wo geklickt wird, oder bearbeitet den angeklickten Text: Tippen Sie,
Eingabe beginnt eine Zeile, die Pfeiltasten, Pos1 und Ende bewegen die
Einfügemarke, und Escape oder ein Klick anderswo beendet das Tippen als eine
Änderung; leer gelassener Text wird entfernt. Die Pipette gibt der Auswahl und
dem als Nächstes Gezeichneten den Stil des angeklickten Objekts. Das
Verlaufswerkzeug legt einen Verlauf entlang des Ziehens auf die Auswahl oder,
wenn nichts gewählt ist, auf das gedrückte Objekt: Umschalt dreht ihn in
15°-Schritten, und Alt, oder ein schon radialer Verlauf, legt ihn radial um
den Punkt, an dem das Ziehen begann; solange das Werkzeug in der Hand ist,
zeigt sich der Verlauf des gewählten Objekts als Linie zwischen zwei Griffen.
Das Zoomwerkzeug vergrößert, wo geklickt wird, verkleinert mit Alt, und ein
damit aufgezogener Rahmen füllt das Fenster. Die Hand verschiebt die Leinwand,
wie es auch ein Ziehen mit der mittleren Taste oder mit gehaltener Leertaste
bei jedem Werkzeug tut.

Auf einen Text gedrückt, setzt das Textwerkzeug die Schreibmarke dorthin, wo
gedrückt wurde. Umschalt mit den Pfeiltasten, Pos1 und Ende oder ein Ziehen
über den Text wählt Zeichen aus, und `Ctrl+A` wählt alle aus; was dann getippt
wird, tritt an ihre Stelle, im Stil des ersten von ihnen. Solange Zeichen
ausgewählt sind, setzt eine im Inspektor gewählte Schrift, Größe, Fettschrift,
Kursive, Laufweite, Anhebung oder Farbe nur sie; ist keines ausgewählt, setzt
sie den ganzen Text, jedes seiner Zeichen. Der Reiter Form hält die Laufweite
eines Textes, wie weit seine Zeichen angehoben sind, als Prozentsatz ihrer
Größe, und seinen Zeilenabstand — oder, bei einem Text entlang eines Pfades,
wie weit entlang des Pfades er beginnt —, und beim Tippen zeigt der Inspektor,
wie das erste ausgewählte Zeichen gesetzt ist. Ein Verlauf malt stets einen
ganzen Text.

Pfad ▸ Text auf Pfad setzen setzt bei einem ausgewählten Text und einer
ausgewählten Form den Text entlang des Umrisses der Form, die Form schließt
sich ihm an: Jedes Zeichen steht auf dem Pfad, in seine Richtung gedreht, ein
Text entlang eines Pfades ist eine Zeile, und ein Zeichen, das über ein Ende
hinausfällt, wird nicht gezeichnet. Vom Pfad lösen setzt den Text wieder in
Zeilen, von dort, wo sein Pfad begann, und gibt den Pfad als eigenes Objekt
zurück. In Pfad umwandeln macht aus einem Text, der auf mehr als eine Weise
gemalt ist, eine Gruppe aus einem Pfad für jede Weise. Das Knotenwerkzeug
bearbeitet den Pfad, entlang dessen ein Text gesetzt ist, wie jeden Pfad.

Das Mausrad verschiebt die Leinwand, mit Umschalt quer; mit Strg zoomt es um
den Zeiger, wie ein Zusammenziehen der Finger auf einem Touchpad oder
Touchscreen. Ansicht ▸ Seite einpassen und Auswahl einpassen passen sie ins
Fenster ein, und Originalgröße zeigt die Zeichnung zu 100 Prozent; der Zoom
reicht von 1 Prozent bis 25600.

Der Inspektor hat fünf Reiter. Stil stellt Füllung und Kontur der Auswahl ein
— und, wenn nichts gewählt ist, des als Nächstes Gezeichneten: Wählen Sie
Füllung oder Kontur, dann Keine oder eine Farbe aus der Farbauswahl, die ein
Quadrat aus Sättigung und Hellwert neben einem Streifen von Farbtönen, ein
Farbrad oder Schieberegler sowie die Werte und die Deckkraft der Farbe bietet;
dann die Breite der Kontur, ihre Ecken, spitz, rund oder abgeschrägt, ihre
Enden, gerade, rund oder eckig, ihre Strichelung, durchgehend, gestrichelt,
gepunktet oder strichpunktiert, die Pfeilspitzen an Anfang und Ende — jeder
Druck schaltet durch keine, Dreieck, Pfeil, Kreis und Raute —, die Füllregel,
Nicht-Null oder Gerade-Ungerade, und die Deckkraft jedes gewählten Objekts.
Ebenen listet die Ebenen, die oberste zuerst: Drücken Sie eine, um in sie zu
zeichnen, und ihre Schaltfläche Sichtbar, um sie zu zeigen oder zu verbergen;
über der Liste fügt Neu eine Ebene hinzu, Entfernen entfernt die Ebene in der
Hand, Sperren schützt sie vor Änderungen, und Höher und Tiefer verschieben
sie. Objekt setzt die Auswahl nach ihrer linken und oberen Kante und skaliert
sie auf eine Breite und Höhe, richtet sie aus — nach linken Kanten, Mitten
oder rechten Kanten, oder nach oben, Mitte oder unten — und verteilt drei oder
mehr Objekte gleichmäßig quer oder abwärts. Objekte richten sich innerhalb
ihrer Auswahl aus; ein einzelnes Objekt, oder jede Anzahl mit An Seite
ausrichten, innerhalb der Seite. Form enthält die Ecken eines Rechtecks, die
Zacken und den Innenradius eines Sterns und ob er ein Vieleck ist, sowie die
Schriftfamilie des Texts — serifenlos, mit Serifen oder dicktengleich —,
Größe, fett, kursiv und die Ausrichtung an seinem Ursprung. Ein Reiter, der
höher ist als das Fenster, rollt. Eine Farbe, die gerade gewählt wird, eine
getippte oder schrittweise geänderte Breite und eine Deckkraft werden auf der
Leinwand gezeigt, während sie sich ändern, und werden eine rückgängig zu
machende Änderung, wenn sie sich setzen; Tab geht zum nächsten Feld und
Umschalt+Tab zum vorigen.

Unter Stil malen Linear und Radial Füllung oder Kontur mit einem Verlauf, der
jedem Objekt eingepasst wird, von seiner Farbe zu Weiß oder, wo sie hell ist,
zu Schwarz. Die Leiste darunter zeigt den Verlauf über einem Schachbrett, mit
einem Griff für jeden Farbstopp: Drücken Sie einen Griff, um diesen Stopp in
die Hand zu nehmen – seine Farbe ändert dann die Farbauswahl –, und ziehen Sie
ihn entlang der Leiste; ein Druck auf die Leiste fügt dort einen Stopp hinzu.
Entf oder Stopp entfernen entfernt den Stopp in der Hand; Links und Rechts
nehmen den Stopp daneben, mit Umschalt verschieben sie ihn um ein Hundertstel.
Stopp bei setzt den Stopp in der Hand, Winkel dreht einen linearen Verlauf,
Auffüllen, Spiegeln und Wiederholen bestimmen, wie ein Verlauf über seine
Enden hinaus weitergeht, und Umkehren lässt ihn andersherum laufen. Unter der
Farbauswahl geben die Farbfelder – Grautöne, dann zwölf Farbtöne voll,
aufgehellt und abgedunkelt, dann die eigenen der Zeichnung – ihre Farbe der
Farbe oder dem Stopp in der Hand; Farbfeld hinzufügen behält die Farbe in der
Hand bei der Zeichnung, Farbfeld entfernen gibt eines ihrer eigenen auf. Unter
Objekt dreht Drehung die Auswahl gegen den Uhrzeigersinn: ein Objekt auf den
angegebenen Winkel, mehrere gemeinsam um ihn.

Seite stellt Breite und Höhe der Seite in ihrer Einheit ein und ob sie hoch
oder quer liegt; die Einheit — px, pt, pc, mm, cm oder in; den Abstand des
Rasters in dieser Einheit und in wie viele Teile jede Seite seiner Quadrate
geteilt ist; Kopie-dpi, die Auflösung, in der ihre PNG-, JPEG- und TIFF-Kopien
entstehen; und den Grund der Seite, Keine oder eine Farbe aus einer eigenen
Farbauswahl, die auf der Leinwand gezeigt wird, während sie gewählt wird. Eine
Seite ohne Grund erscheint als weißes Papier, und eine Kopie als PNG ist
überall durchsichtig, wo nichts gezeichnet ist.

Ein Ziehen aus einem Lineal setzt eine Hilfslinie quer oder längs über die
Leinwand; ziehen Sie eine Hilfslinie, um sie zu verschieben, oder zurück auf
ein Lineal, um sie zu entfernen. Ansicht ▸ Raster zeigt das Raster der Seite.
Solange Fangen an ist, rastet der Zeiger ein, wenn eine Form gezeichnet, ein
Federknoten gesetzt oder die Auswahl verschoben oder skaliert wird: an
Hilfslinien, an den Kanten und der Mitte der Seite und am Raster, solange es
zu sehen ist. Ansicht ▸ Umrisse zeigt jedes Objekt als seinen Umriss, was eine
dichte Zeichnung lesbar macht. Seite einrichten im Menü Seite zeigt den Reiter
Seite, und das Menü stellt das Papier der Seite ein — A4, A3, A5, A6, A2, A1,
A0, B5, B4, US Letter, US Legal, Tabloid, Executive, zwei Bildschirmgrößen und
ein Symbol —, dreht sie zwischen Hoch- und Querformat, gibt ihr den Grund des
Reiters Seite oder keinen und stellt ihre Einheiten ein: Pixel, Punkt, Pica,
Millimeter, Zentimeter oder Zoll.

Objekt ▸ Gruppieren fasst die Auswahl zu einer Gruppe zusammen, und Gruppierung
aufheben löst sie; Ganz nach vorn, Höher, Tiefer und Ganz nach hinten ordnen
Objekte in ihrer Ebene um; Spiegeln und Drehen spiegeln die Auswahl oder drehen
sie um eine Vierteldrehung. Sperren und Verbergen nehmen Objekte aus der
Reichweite, und Alle in der Ebene entsperren und Alle in der Ebene zeigen holen
die der Ebene in der Hand zurück. Das Menü Pfad vereint die gewählten Formen zu
einer (Vereinigung), schneidet die oberen aus der untersten (Differenz),
behält, wo sie sich überlappen (Schnittmenge) oder wo nicht (Ausschluss),
wandelt Formen und Text in Pfade, deren Knoten sich bearbeiten lassen, und
Konturen in gefüllte Umrisse. Diese werden abseits des Fensters berechnet, das
dabei frei bleibt; eine Zeichnung, die geändert wird, bevor das Ergebnis
eintrifft, behält ihre Änderung, und das Ergebnis wird nicht angewendet.

Objekt ▸ Auf oberstes Objekt beschneiden zeigt den Rest der Auswahl nur
innerhalb ihres obersten Objekts, und Beschneidung aufheben zeigt eine
beschnittene Gruppe wieder ganz, das beschneidende Objekt wieder obenauf. Pfad
▸ Teilen zerschneidet die gewählten Formen in die Stücke, die ihre
Überlappungen bilden. Erweitern und Verkleinern vergrößern oder verkleinern
jede gewählte Form um den Versatz des Reiters Form, Vereinfachen zeichnet jede
mit so wenigen Knoten neu, wie sie innerhalb der Glätte des Reiters halten,
und Richtung umkehren lässt einen Pfad andersherum laufen, sodass seine
Pfeilspitzen und Striche ihm folgen.

Jede Änderung ist ein Schritt zum Rückgängigmachen, und Rückgängig und
Wiederholen nennen den Schritt, den sie tun; bis zu 1000 Schritte werden
behalten, weniger, wenn Speicher knapp ist. Kopieren legt die Auswahl als SVG in
die Zwischenablage, und Einfügen fügt den Inhalt der Zwischenablage der Ebene in
der Hand hinzu, an seiner Stelle auf der Seite, ausgewählt.

Datei ▸ Bild platzieren (`Ctrl+Shift+P`) fragt nach einem Bild — einer PNG-,
JPEG-, GIF-, BMP-, TIFF-, WebP-, Windows-Symbol- oder OpenRaster-Datei oder
einer RISC-OS-Sprite-Datei, bei einer Datei mit mehreren deren erste Seite
oder erstes Sprite — und legt es in einem Schritt in die Ebene in der Hand,
ausgewählt und nach seiner Datei benannt, mitten in das, was die Leinwand
zeigt: in seiner eigenen Größe oder, wo es größer ist, auf vier Fünftel der
Leinwand verkleinert. Die Datei wird nur gelesen, nie verändert. Ein Bild wird
wie jedes Objekt verschoben, skaliert, gedreht, beschnitten und kopiert; es
hat keinen eigenen Stil und keine Knoten, und wo Formen vereinigt, geschnitten
oder geteilt werden, nimmt es als sein Rechteck teil. Es wird mit der
Zeichnung gespeichert: SVG bettet es ein, eine PDF- oder EPS-Kopie zeichnet
es, und eine Draw-Datei hält es als Sprite oder JPEG. Die Bilder einer
Zeichnung fassen zusammen höchstens 268 Millionen Pixel, jedes einmal gezählt,
wie oft es auch gezeigt wird; ein Bild, das sie darüber hinaus brächte, wird
abgelehnt, und das Statusband sagt, warum.

Einstellungen im Menü der Symbolleiste öffnet das Einstellungsfenster von
Draw: das Werkzeug, mit dem ein neues Fenster beginnt, und ob es das Raster
zeigt, einrastet und nur Umrisse zeichnet; Papier, Ausrichtung, Einheiten und
Raster, mit denen eine neue Zeichnung beginnt; Füllung und Kontur, mit denen
neue Objekte gezeichnet werden, jeweils keine oder eine Farbe, und die Breite
der Kontur; wie weit die Pfeiltasten verschieben, ohne Einstellung ein Pixel;
wie weit ein Duplikat von seinem Original absteht; und wie nah der Zeiger
einem Griff, einem Knoten oder einer Hilfslinie kommen muss, um sie zu fassen.
Eine Änderung gilt sofort und bleibt für das nächste Mal; Standard
wiederherstellen stellt alles zurück.

Draw hält keine Fähigkeit für das Dateisystem. Es bearbeitet nur die Datei,
die ihm übergeben wurde: Eine Datei, die der Benutzer ändern darf, wird
beschreibbar übergeben, und Speichern schreibt sie zurück; jede andere öffnet
sich schreibgeschützt, und Speichern fragt, wohin. Dokumente, aus der
Zwischenablage eingefügte Zeichnungen und platzierte Bilder werden in einem
getrennten Arbeitsprozess ohne jede Reichweite gelesen, sodass eine feindliche
Datei nichts berühren kann, was Draw erreichen kann.

Ein Druck auf die sekundäre (rechte) Maustaste irgendwo im Fenster öffnet sein
Menü: Datei, Seite, Bearbeiten, Objekt, Pfad, Ebene, Ansicht und Werkzeuge, jedes
ein Untermenü. Das Fenster hat keine Menüleiste. Ein Fenster zu schließen oder zu
beenden, während Änderungen nicht gespeichert sind, fragt zuerst. Ein Fenster mit
der sekundären Taste auf seiner Schließen-Schaltfläche zu schließen, zeigt
außerdem seine Datei in der Dateiverwaltung, im Ordner, der sie enthält.

* `Ctrl+N` — ein neues Fenster; `Ctrl+O` — eine Datei öffnen; `Ctrl+Shift+P` — ein Bild platzieren
* `Ctrl+S` — speichern; `Ctrl+Shift+S` — speichern unter; `Ctrl+Shift+E` — eine Kopie speichern
* `Ctrl+W` — das Fenster schließen
* `Ctrl+Z` — rückgängig; `Ctrl+Shift+Z` oder `Ctrl+Y` — wiederholen
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — ausschneiden, kopieren, einfügen; `Ctrl+D` — duplizieren
* `Delete` oder `Backspace` — die Auswahl löschen
* `Ctrl+A` — alles auswählen; `Ctrl+Shift+A` — nichts auswählen; `Escape` — nichts auswählen oder den gesetzten Pfad verwerfen
* `Ctrl+G` — gruppieren; `Ctrl+Shift+G` — Gruppierung aufheben
* `Page Up` / `Page Down` — höher oder tiefer; `Home` / `End` — ganz nach vorn oder ganz nach hinten
* `Ctrl+[` / `Ctrl+]` — nach links oder rechts drehen
* `Ctrl+L` — sperren; `Ctrl+Shift+L` — alle in der Ebene entsperren
* `Ctrl+H` — verbergen; `Ctrl+Shift+H` — alle in der Ebene zeigen
* `Ctrl+Shift+C` — Objekt in Pfad
* `Ctrl+Shift+N` — eine neue Ebene; `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — die Ebene höher oder tiefer
* `Ctrl+=` oder `Ctrl++` / `Ctrl+-` — vergrößern oder verkleinern; `Ctrl+1` — Originalgröße; `Ctrl+0` — die Seite einpassen
* `Ctrl+'` — das Raster zeigen oder verbergen; `%` — fangen oder nicht
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `G`, `I`, `Z`, `H` — die Werkzeuge, der Reihe nach
* Pfeiltasten — die Auswahl so weit verschieben, wie die Einstellungen sagen, ohne Einstellung ein Pixel; mit `Shift` zehnmal so weit
* `Space` — gehalten, die Leinwand mit jedem Werkzeug ziehen
* `Ctrl` + Mausrad — um den Zeiger zoomen; `Shift` + Mausrad — quer rollen

## OPTIONS

`-h`, `-?`, `--help`
: Schreibt diese Hilfe auf die Standardausgabe und beendet sich.

## EXIT STATUS

Null nach Beenden. Ungleich null, wenn der Fensterkanal, das Ereignispostfach
oder die Desktop-Sitzung verweigert wurde; der Grund wird auf der
Standardfehlerausgabe genannt.
