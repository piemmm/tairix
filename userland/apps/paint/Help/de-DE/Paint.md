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
WebP, Windows-Symbole und RISC-OS-Sprite-Dateien. Es schreibt PNG, JPEG und
Sprite-Dateien; ein Bild aus einem anderen Format wird als neue Datei
gespeichert. Ein PNG behält seine Palette, und ein JPEG wird in der Qualität
geschrieben, die unter JPEG-Qualität im Menü Datei eingestellt ist.

Eine Sprite-Datei enthält beliebig viele Sprites, jedes mit Namen,
Bildschirmmodus, Palette und Maske. Jede Farbtiefe wird so bearbeitet, wie
sie gespeichert ist: 2, 4, 16 und 256 Farben sowie Millionen Farben. Ein
Sprite ohne eigene Palette zeigt die Farben des RISC-OS-Desktops — bei 16
Farben ist Farbe n die Wimp-Farbe n; bei 2 Farben die Wimp-Farben 0 und 7;
bei 4 Farben die Wimp-Farben 0, 2, 4 und 7; bei 256 Farben die
RISC-OS-Tönungsanordnung — niemals eine PC-Palette. Ein Sprite, dessen
Pixel höher als breit sind, wie im Modus 12, wird so dargestellt. Ein
Sprite, das dieser Editor nicht lesen kann, etwa ein CMYK-Sprite, bleibt
genau so erhalten und wird unverändert zurückgeschrieben. Das Menü Sprites
springt zu Sprites, fügt sie hinzu, kopiert, benennt um, löscht und ordnet
sie neu.

Die primäre (linke) Taste malt mit der Primärfarbe und die mittlere Taste
mit der Sekundärfarbe; mit gedrückter Alt-Taste wird stattdessen eine Farbe
aufgenommen. Die Werkzeuge sind Auswahl, Stift, Pinsel, Sprühdose,
Radierer, Füllen, Farbpipette, Linie, Rechteck und Ellipse; das Feld neben
dem Bild enthält die Palette des Bildes oder die Desktop-Farben und die
Einstellungen des gewählten Werkzeugs. Das Farbdock rechts enthält Primär-
und Sekundärfarbe und einen Farbwähler für die gewählte der beiden: ein
Klick auf eine Farbe wählt sie, dann wird sie über Farbton, Sättigung und
Hellwert, über Rot, Grün und Blau, über ihre hexadezimale Schreibweise und,
wo das Bild Transparenz hält, über ihre Deckkraft eingestellt. Daneben
steht die Farbe, die sie hatte, und ein Klick stellt sie wieder her. In
einem Bild mit Palette sind die Farben ihre Einträge, der Farbwähler
bearbeitet also die Palette, und jede Bearbeitung ist ein Schritt, der sich
rückgängig machen lässt. Mit gedrückter Umschalttaste entsteht ein Quadrat,
ein Kreis oder eine Linie im Vielfachen von 45 Grad.

Mit dem Auswahlwerkzeug zieht man einen Teil des Bildes auf und verschiebt
die Auswahl dann durch Ziehen; sie schwebt, bis sie abgelegt wird, und das
Verschieben ist ein einziger rückgängig zu machender Schritt. Kopierte
Bilder gehen als PNG über die Zwischenablage, und Eingefügtes schwebt, bis
es abgelegt wird.

Das Programm besitzt keine Dateisystem-Berechtigung. Es bearbeitet nur die
Datei, die ihm übergeben wurde. Eine Datei, die der Benutzer ändern darf,
wird beschreibbar übergeben, und Speichern schreibt sie zurück; jede andere
ist schreibgeschützt, und Speichern fragt, wohin eine Kopie gespeichert
werden soll. Bilder, auch aus der Zwischenablage eingefügte, werden in einem
eigenen Arbeitsprozess ohne jede Reichweite decodiert, und jedes Dokument
erhält einen neuen: Eine bösartige Datei kann nichts erreichen, was das
Programm erreichen kann.

Die sekundäre (rechte) Maustaste öffnet überall im Fenster dessen Menü:
Ausschneiden, Kopieren, Einfügen, Alles auswählen und Auswahl aufheben,
dann Datei, Bearbeiten, Bild, Farben, Sprites, Ansicht und Werkzeuge, jedes
mit eigenem Untermenü. Das Fenster hat keine Menüleiste. Das Schließen
eines Fensters oder Beenden mit ungespeicherten Änderungen fragt zuerst
nach.

* `Ctrl+N` — ein neues Bild; `Ctrl+O` — eine Datei öffnen
* `Ctrl+S` — speichern; `Ctrl+Shift+S` — speichern unter
* `Ctrl+W` — das Fenster schließen
* `Ctrl+Z` — rückgängig; `Ctrl+Shift+Z` oder `Ctrl+Y` — wiederholen
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — ausschneiden, kopieren, einfügen
* `Ctrl+A` — alles auswählen; `Ctrl+D` — Auswahl aufheben
* `Enter` — eine schwebende Auswahl ablegen; `Escape` — sie zurücklegen
* `Delete` — die Auswahl löschen
* `Ctrl+Shift+X` — auf die Auswahl zuschneiden
* `Ctrl+R` — Größe ändern; `Ctrl+Shift+R` — Leinwandgröße
* `Ctrl+[` / `Ctrl+]` — nach links oder rechts drehen
* `Ctrl+I` — die Farben umkehren
* `S`, `P`, `B`, `A`, `E`, `F`, `I`, `L`, `R`, `O` — die Werkzeuge der Reihe nach
* `X` — Primär- und Sekundärfarbe tauschen
* `Tab` — ins Farbdock und durch seine Teile; `Escape` — zurück zum Bild
* `+` / `-` — vergrößern oder verkleinern; `1` — Originalgröße; `Ctrl+0` — einpassen
* `Ctrl` + Mausrad — um den Zeiger herum vergrößern oder verkleinern
* Zwei Finger spreizen oder zusammenführen — stufenlos vergrößern oder verkleinern; auf einem Touchscreen folgt das Bild den Fingern
* `G` — das Raster zwischen den Pixeln ein- oder ausblenden
* `Page Up` / `Page Down` — das vorige oder nächste Sprite
* Pfeiltasten — eine schwebende Auswahl um ein Pixel verschieben; mit `Shift` um zehn

## OPTIONS

`-h`, `-?`, `--help`
: Schreibt diese Hilfe auf die Standardausgabe und beendet sich.

## EXIT STATUS

Null nach Beenden. Nicht null, wenn der Fensterkanal, das
Ereignispostfach oder die Desktop-Sitzung verweigert wurde; der Grund wird
auf der Standardfehlerausgabe genannt.
