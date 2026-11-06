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

Gestartet wird es über die Zeile *Settings…* im Systemmenü des Desktops oder
aus einer Shell über seinen Pfad `/System/Components/settings.app`. Es
benötigt eine laufende grafische Sitzung: ohne sie ist der Fensterkanal
unerreichbar, und es meldet die Ablehnung auf dem Standardfehlerstrom und
beendet sich.

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
Administrator festgelegt. Ändern Sie die Lautstärke des Geräts, über das der
Ton spielt — hier oder über die Symbolleiste —, erklingt ein kurzer Ton in der
neuen Lautstärke: wo ein Ziehen zur Ruhe kommt, bei jedem Schritt mit den
Pfeiltasten und beim Aufheben der Stummschaltung, damit Sie hören, wie laut es
jetzt ist. In *Sound Effects* schaltet
*Play feedback when the volume changes* ihn aus. Er folgt nicht den übrigen
Klängen des Desktops: Er spielt in der eigenen Lautstärke des Geräts, ob diese
ein- oder ausgeschaltet sind.

**Raspberry Pi** erscheint nur, wenn der Rechner einer ist. Dort wird der Lüfter
des Prozessors eingestellt, der über eine Leitung der 40-poligen Stiftleiste
geschaltet wird: *Off*, ständig *On* oder *Automatic*, das den Lüfter
einschaltet, sobald der Prozessor die eingestellte Temperatur erreicht, und erst
wieder aus, wenn er um den eingestellten Abstand abgekühlt ist, damit er nicht
um ein Grad herum hin- und herschaltet. Die Steuerleitung wählen Sie über ihre
GPIO-Nummer und ihren Stift, dazu, ob der Lüfter bei hohem oder niedrigem Pegel
läuft; eine Leitung, die schon anderweitig belegt ist, wird genannt statt
angeboten. Eine Leitung der Stiftleiste kann einen Lüfter nicht selbst
versorgen, schließen Sie ihn also über einen Transistor an. Der Bereich zeigt
außerdem die Temperatur des Prozessors und ob der Lüfter läuft. Kann *Automatic*
die Temperatur nicht lesen, läuft der Lüfter, bis es wieder gelingt. Wie die
übrigen Einstellungen des Rechners wartet eine Änderung auf *Apply*, das nach
einem Konto fragt, das sie ändern darf.

**Storage** listet jeden eingehängten Datenträger auf, außer dem, den ein
anderer Benutzer privat hält: wo er eingehängt ist, sein Dateisystem, sein Gerät
und sein Medium, wie voll er ist und ob er in gutem Zustand ist. Es handelt sich
um einen Bericht, nicht um ein Bedienelement — Ein- und Aushängen sind Sache des
Dateimanagers und des Befehls `mount` —, und ein Datenträger, dessen Format
keine feste Kapazität kennt, sagt das, statt einen Balken zu zeigen. `df` meldet
dieselben Zahlen in einer Shell.

**Files** legt fest, wie der Dateimanager arbeitet: wo sich ein neues Fenster
öffnet und wie es anzeigt und ordnet, was es auflistet, ob es die Orte-Leiste
und die Werkzeugleiste zeigt, ob Ordner oben bleiben, welche Namensendungen und
welche Einträge ausgeblendet sind, ob ein Bild seinen eigenen Inhalt zeigt und
ein Ordner, was er enthält, ob vor dem Verschieben von Einträgen in den
Papierkorb nachgefragt wird und ob ein Datenträgerabbild oder Archiv, das Sie
geöffnet haben, ausgehängt wird, sobald seine Fenster geschlossen sind. Die
Namensendungen, die er ausblendet, sind auch auf dem Desktop ausgeblendet.
*Show on the desktop* hat für jede Art von Laufwerk ein Kästchen —
*The system drive*, *Fixed drives*, *Removable drives*, *Network shares* und
*Disk images and archives* —, und der Desktop zeigt für jedes angeschlossene
Laufwerk einer angekreuzten Art ein Symbol, das es öffnet, wie der Dateimanager
es tut; anfangs sind nur Wechseldatenträger angekreuzt. Eine
Zeile wirkt sofort, wenn Sie sie wählen: Wo sich ein neues Fenster öffnet und ob
es die Orte-Leiste und die Werkzeugleiste zeigt, gilt für das nächste Fenster,
das Sie öffnen, und alles andere erreicht den Desktop und alle
Dateimanager-Fenster zugleich; ein Fenster behält jedoch eine Anzeige oder eine
Reihenfolge, die Sie selbst
darin gewählt haben. *Settings…* im eigenen Menü des Dateimanagers öffnet diesen
Bereich.

## EXIT STATUS

Null nach einem saubereren Schließen; ungleich null, wenn der Fensterkanal oder
der gemeinsame Rahmenbereich abgelehnt wurde (der Grund steht auf dem
Standardfehlerstrom).
