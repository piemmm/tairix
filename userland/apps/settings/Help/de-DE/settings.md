## NAME

settings — den Desktop und diesen Rechner einrichten

## SYNOPSIS

`settings`

## DESCRIPTION

Öffnet ein Desktop-Fenster, das jede Einstellungskategorie dieses Systems
auflistet: was der Rechner ist, wie der Desktop aussieht, der Bildschirm, das
Netzwerk, die Eingabegeräte, die Konten, die ihn benutzen, und die Datenträger,
die er enthält. Die Auswahl einer Kategorie in der Seitenleiste zeigt deren
Bereich. Eine Kategorie mit mehreren Bereichen öffnet und schließt stattdessen
deren Liste an Ort und Stelle, und beliebig viele dieser Listen können
gleichzeitig geöffnet sein.

Settings besitzt keine eigene Befugnis. Jede Änderung ist entweder eine Anfrage
an die Desktop-Sitzung, welcher die Einstellungen des Benutzers gehören, oder
ein erneut authentifizierter Aufruf des Befehls, der diesen Speicher bereits
schreibt; nichts hier kann Rechte ausweiten.

Eine Kategorie, die dieses System nicht bedienen kann, sagt das deutlich und
nennt, was dafür vorhanden sein müsste. Ein Bedienelement, das nichts ändern
würde, wird niemals gezeigt.

Tippen Sie in das Suchfeld über der Seitenleiste, um sie auf die Kategorien und
Einstellungen zu filtern, die ein Wort erreicht. `Tab` und `Shift+Tab` wechseln
zwischen Suchfeld, Pfadleiste, Seitenleiste und Bereich; `Up` und `Down` laufen
durch die Seitenleiste und `Enter` öffnet die Zeile. `Right` und `Left` öffnen
und schließen die Liste der Kategorie unter dem Cursor. Ein zu schmales Fenster
lässt die Seitenleiste weg, und der erste Eintrag der Pfadleiste listet dann die
Kategorien auf.

Gestartet wird es über die Zeile *Settings…* im Systemmenü des Desktops, über
die Programmbibliothek oder namentlich aus einer Shell. Es benötigt eine
laufende grafische Sitzung: ohne sie ist der Fensterkanal unerreichbar, und es
meldet die Ablehnung auf dem Standardfehlerstrom und beendet sich.

Ton hat zwei Bereiche. *Output & Input* wählt das Gerät, über das der Ton
spielt, und das, von dem aufgenommen wird, und stellt Lautstärke und
Stummschaltung dieses Geräts ein: Stummschalten dreht die Lautstärke ganz
herunter, bis Sie es aufheben, und dann steht sie wieder, wo sie war. Er zeigt
auch, was aufnimmt: Ihre eigenen Aufnahmen mit Namen und wie viele die anderen
machen. Das ändert das Gerät sofort, und nur solange Ihre Sitzung den Raum
hält, den das Gerät bedient; ein Gerät im Raum einer anderen Sitzung wird
angezeigt, lässt sich hier aber nicht ändern. Ihr Desktop merkt sich, was Sie
einstellen. *Sound Effects* schaltet die eigenen Klänge des Desktops ein oder
aus, legt ihr Thema und ihre Lautstärke fest und bestimmt für jedes Ereignis —
eine Benachrichtigung, einen Fehler, die Terminalglocke —, ob es den Klang
seines Themas, eine von Ihnen gewählte Datei oder nichts abspielt. Die Klänge
beim Starten, An- und Abmelden gehören dem Rechner und werden von seinem
Administrator festgelegt.

## EXIT STATUS

Null nach einem saubereren Schließen; ungleich null, wenn der Fensterkanal oder
der gemeinsame Rahmenbereich abgelehnt wurde (der Grund steht auf dem
Standardfehlerstrom).
