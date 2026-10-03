## NAME

Paint — grafischer Bild- und Sprite-Editor

## SYNOPSIS

`Paint`

## DESCRIPTION

Malt und bearbeitet Bilder in einem Desktop-Fenster, Pixel für Pixel oder
mit Pinseln und Formen. Mit einem Dokument gestartet — aus der
Dateiverwaltung, vom Desktop oder durch Ablegen einer Datei auf seinem
Symbol in der Symbolleiste — öffnet es ein Fenster darauf. Allein gestartet
öffnet es ein neues, weißes Bild. Jedes Dokument ist ein Fenster des einen
Programms; das Schließen des letzten lässt es in der Symbolleiste, und die
Zeile Beenden seines Symbolmenüs beendet es.

Es öffnet jedes Bildformat, das das System liest: PNG, JPEG, GIF, BMP, TIFF,
WebP, Windows-Symbole, RISC-OS-Sprite-Dateien und OpenRaster. Es schreibt
PNG, JPEG, GIF, BMP, TIFF, Sprite-Dateien und OpenRaster; ein Bild aus einem
anderen Format oder aus einer Datei, die mehr als ihr Bild enthält, etwa ein
Farbprofil, wird als neue Datei gespeichert. Neues Bild fragt zuerst, für
welches Format das Bild gedacht ist, und bietet die Farben an, die dieses
Format fasst. Speichern unter fragt nach dem Format und dessen eigenen
Einstellungen — der Qualität eines JPEG, ob ein GIF mit Zeilensprung
geschrieben wird, der Kompression eines TIFF — und sagt, was das Format
nicht behalten kann, bevor es nach dem Ort fragt. Eine Palette bleibt
erhalten, wo immer das Format eine fasst, ebenso die Auflösung eines Bildes.

Ein Bild kann aus Ebenen bestehen, die unterste zuerst, jede mit Namen,
Deckkraft und der Angabe, ob sie sichtbar ist; gemalt wird immer auf einer
Ebene, und das Fenster zeigt sie übereinandergelegt. OpenRaster behält die
Ebenen; jedes andere Format erhält sie übereinandergelegt. Das Menü Ebenen
fügt Ebenen hinzu, kopiert, löscht, hebt und senkt sie, vereint eine mit der
darunter und reduziert das Bild auf eine Ebene, und seine
Ebeneneigenschaften benennen eine Ebene um und legen fest, wie viel von ihr
zu sehen ist. Korrekturen, Füllungen und Striche ändern die Ebene, auf der
gemalt wird; Drehen, Spiegeln, Größe ändern und Zuschneiden ändern jede
Ebene. Ein Bild mit Palette hat eine einzige Ebene.

Eine Sprite-Datei enthält beliebig viele Sprites, jedes mit Namen,
Bildschirmmodus, Palette und Maske. Jede Farbtiefe wird so bearbeitet, wie
sie gespeichert ist: 2, 4, 16 und 256 Farben sowie Millionen Farben. Ein
Sprite ohne eigene Palette zeigt die Farben des RISC-OS-Desktops — bei 16
Farben ist Farbe n die Wimp-Farbe n; bei 2 Farben die Wimp-Farben 0 und 7;
bei 4 Farben die Wimp-Farben 0, 2, 4 und 7; bei 256 Farben die
RISC-OS-Tönungsanordnung — niemals eine PC-Palette. Ein Sprite, dessen Pixel
höher als breit sind, wie im Modus 12, wird so dargestellt. Ein Sprite, das
dieser Editor nicht lesen kann, etwa ein CMYK-Sprite, bleibt genau so
erhalten und wird unverändert zurückgeschrieben. Das Menü Sprites springt zu
Sprites, fügt sie hinzu, kopiert, benennt um, löscht und ordnet sie neu.
Eine TIFF-Datei enthält beliebig viele Seiten; für sie springt das Menü
Seiten zu Seiten, fügt sie hinzu, kopiert, löscht und ordnet sie neu.

Die primäre (linke) Taste malt mit der Primärfarbe und die mittlere Taste
mit der Sekundärfarbe; mit gedrückter Alt-Taste wird stattdessen eine Farbe
aufgenommen, so wie die Ebenen sie zeigen. Die Werkzeuge im Werkzeugkasten
links sind Auswahl, Stift, Pinsel, Airbrush, Radierer, Klonen, Füllen,
Verlauf, Farbpipette, Text, Linie, Rechteck, Ellipse, Polygon, Zuschneiden,
Hand und Zoom. Die Leiste oben nennt das gewählte Werkzeug und enthält seine
Einstellungen — Größe, Härte, Deckkraft, Fluss und Abstand eines Pinsels,
die Toleranz einer Füllung, die Form eines Verlaufs, die Textgröße, die
Ecken eines Rechtecks —, getippt oder mit den Pfeiltasten verstellt, und die
Schaltflächen zum Zoomen und für das Pixelraster; der Palettenstreifen unter
dem Bild enthält die Palette des Bildes oder die Desktop-Farben. Der
Airbrush sprüht weiter, solange er still gehalten wird. Mit gedrückter
Umschalttaste entsteht ein Quadrat, ein Kreis oder eine Linie im Vielfachen
von 45 Grad.

Das Auswahlwerkzeug zieht ein Rechteck, eine Ellipse, ein freihändiges
Lasso oder ein Polygon Ecke für Ecke auf, oder es wählt mit dem Zauberstab
die Pixel, die über ähnliche Farben mit einem verbunden sind. Mit gedrückter
Umschalttaste wird zur Auswahl hinzugefügt, mit Alt davon abgezogen, mit
beiden bleibt nur, was beiden gemeinsam ist; Weiche Kante macht ihren Rand
weich. Solange eine Auswahl besteht, sind alle Werkzeuge, Füllungen und
Korrekturen an sie gebunden. Ziehen in ihrem Inneren hebt sie an und
verschiebt sie: Sie schwebt, bis sie abgelegt wird, und das Verschieben ist
ein einziger rückgängig zu machender Schritt. Kopierte Bilder gehen als PNG
über die Zwischenablage, und Eingefügtes schwebt, bis es abgelegt wird.

Das Klonwerkzeug malt, was an anderer Stelle im Bild liegt: Mit Alt-Klick
den Ursprung wählen, dann malen. Das Verlaufswerkzeug geht entlang einer
Ziehbewegung von der Primär- in die Sekundärfarbe über, in Streifen oder in
Ringen. Das Textwerkzeug setzt die getippten Worte an die angeklickte
Stelle; Enter beginnt eine neue Zeile, ein weiterer Klick oder ein anderes
Werkzeug legt sie ab, und Escape verwirft sie. Die Ecken des
Polygonwerkzeugs werden der Reihe nach angeklickt, und ein Klick auf die
erste oder Enter schließt es. Das Zuschneidewerkzeug markiert den Teil, der
bleibt, seine Griffe verschieben seine Kanten, und Enter schneidet zu. Die
Hand schiebt das Bild durch das Fenster, wie die Leertaste mit jedem
Werkzeug; das Zoomwerkzeug vergrößert per Klick, mit Alt verkleinert es,
und ein aufgezogener Rahmen füllt das Fenster.

Das Menü Korrekturen ändert Helligkeit und Kontrast, Farbton und Sättigung
und die Tonwerte, posterisiert, setzt einen Schwellwert, entsättigt,
zeichnet weich, schärft, verpixelt, fügt Rauschen hinzu und findet Kanten;
jede Korrektur zeigt sich am Bild, während ihre Einstellungen sich bewegen,
und bleibt nur, wenn sie angewendet wird. Bei einem Bild mit Palette ändert
eine Korrektur dessen Palette, und solche, die benachbarte Farben brauchen,
werden nicht angeboten.

Das Farbdock rechts enthält Primär- und Sekundärfarbe und einen Farbwähler
für die gewählte der beiden: Ein Klick auf eine Farbe wählt sie, dann wird
sie über Farbton, Sättigung und Hellwert, über Rot, Grün und Blau, über ihre
hexadezimale Schreibweise und, wo das Bild Transparenz hält, über ihre
Deckkraft eingestellt. Daneben steht die Farbe, die sie hatte, und ein Klick
stellt sie wieder her. In einem Bild mit Palette sind die Farben ihre
Einträge, der Farbwähler bearbeitet also die Palette, und jede Bearbeitung
ist ein Schritt, der sich rückgängig machen lässt.

Das Programm besitzt keine Dateisystem-Berechtigung. Es bearbeitet nur die
Datei, die ihm übergeben wurde. Eine Datei, die der Benutzer ändern darf,
wird beschreibbar übergeben, und Speichern schreibt sie zurück; jede andere
ist schreibgeschützt, und Speichern fragt, wohin eine Kopie gespeichert
werden soll. Bilder, auch aus der Zwischenablage eingefügte, werden in einem
eigenen Arbeitsprozess ohne jede Reichweite decodiert, und jedes Dokument
erhält einen neuen: Eine bösartige Datei kann nichts erreichen, was das
Programm erreichen kann.

Die sekundäre (rechte) Maustaste öffnet überall im Fenster dessen Menü:
Ausschneiden, Kopieren, Einfügen, Alles auswählen und Auswahl aufheben, dann
Datei, Bearbeiten, Bild, Ebenen, Farben, Korrekturen, Sprites oder Seiten,
Ansicht und Werkzeuge, jedes mit eigenem Untermenü. Das Fenster hat keine
Menüleiste. Das Schließen eines Fensters oder Beenden mit ungespeicherten
Änderungen fragt zuerst nach.

* `Ctrl+N` — ein neues Bild; `Ctrl+O` — eine Datei öffnen
* `Ctrl+S` — speichern; `Ctrl+Shift+S` — speichern unter
* `Ctrl+W` — das Fenster schließen
* `Ctrl+Z` — rückgängig; `Ctrl+Shift+Z` oder `Ctrl+Y` — wiederholen
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — ausschneiden, kopieren, einfügen
* `Ctrl+A` — alles auswählen; `Ctrl+D` — Auswahl aufheben
* `Enter` — eine schwebende Auswahl ablegen, ein Polygon schließen oder zuschneiden; `Escape` — es zurücknehmen
* `Delete` — die Auswahl löschen; `Alt+Backspace` — sie mit der Primärfarbe füllen
* `Ctrl+Shift+X` — auf die Auswahl zuschneiden
* `Ctrl+R` — Größe ändern; `Ctrl+Shift+R` — Leinwandgröße
* `Ctrl+[` / `Ctrl+]` — nach links oder rechts drehen
* `Ctrl+I` — die Farben umkehren
* `Ctrl+Shift+N` — eine neue Ebene; `Ctrl+E` — nach unten vereinen; `Ctrl+Shift+E` — auf eine Ebene reduzieren
* `Ctrl+Page Up` / `Ctrl+Page Down` — auf der Ebene darüber oder darunter malen
* `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — die Ebene heben oder senken
* `S`, `P`, `B`, `A`, `E`, `C`, `F`, `D`, `I`, `T`, `L`, `R`, `O`, `Y`, `K`, `H`, `Z` — die Werkzeuge der Reihe nach
* `Space` — gehalten, das Bild mit jedem Werkzeug verschieben
* `X` — Primär- und Sekundärfarbe tauschen
* `Tab` — durch die Einstellungen des Werkzeugs, den Palettenstreifen und das Farbdock; `Shift+Tab` — zurück; `Escape` — zurück zum Bild
* `+` / `-` — vergrößern oder verkleinern; `1` — Originalgröße; `Ctrl+0` — einpassen
* `Ctrl` + Mausrad — um den Zeiger herum vergrößern oder verkleinern
* Zwei Finger spreizen oder zusammenführen — stufenlos vergrößern oder verkleinern; auf einem Touchscreen folgt das Bild den Fingern
* `G` — das Raster zwischen den Pixeln ein- oder ausblenden
* `Page Up` / `Page Down` — das vorige oder nächste Sprite, die vorige oder nächste Seite
* Pfeiltasten — eine schwebende Auswahl um ein Pixel verschieben; mit `Shift` um zehn; im Palettenstreifen durch seine Farben gehen

## OPTIONS

`-h`, `-?`, `--help`
: Schreibt diese Hilfe auf die Standardausgabe und beendet sich.

## EXIT STATUS

Null nach Beenden. Nicht null, wenn der Fensterkanal, das
Ereignispostfach oder die Desktop-Sitzung verweigert wurde; der Grund wird
auf der Standardfehlerausgabe genannt.
