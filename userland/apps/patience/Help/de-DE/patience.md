## NAME

patience — Klondike, FreeCell und Spider, mit einem Löser hinter jedem Tipp

## SYNOPSIS

`patience`

## DESCRIPTION

Öffnet ein Fenster auf dem Schreibtisch, ausgelegt für eine Patience, jene
Kartenspiele, die auch Solitär heißen. Sechs Spiele stehen im Menü Game, und die
Tasten `1` bis `6` wählen sie: Klondike mit einer oder drei gezogenen Karten,
FreeCell und Spider mit einer, zwei oder vier Farben.

Bei Klondike bauen Sie die sieben Spalten absteigend in wechselnden Farben und
legen jede Farbe auf ihrer Ablage ab, vom Ass bis zum König. Klicken Sie auf den
Stapel, um Karten auf den Abwurf umzudrehen; ist er leer, klicken Sie auf seinen
Platz, um den Abwurf wieder umzudrehen. Bei FreeCell liegen alle Karten offen,
und jede der vier freien Zellen nimmt eine Karte auf; eine Reihe zieht als
Ganzes über die freien Zellen und leeren Spalten, solange sie Platz für sie
haben. Bei Spider bauen Sie absteigend in beliebiger Farbe, ziehen eine Reihe
aber nur, wenn sie ganz in einer Farbe ist; eine Reihe vom König bis zum Ass
wandert von selbst auf die Ablage. Klicken Sie auf den Stapel, um auf jede
Spalte eine Karte zu geben, wozu zuvor jede Spalte eine Karte tragen muss.

Ziehen Sie Karten, wohin Sie wollen, oder klicken Sie sie an: Ein Klick schickt
Karten an den besten Platz, den sie erreichen können, zuerst auf die Ablage,
dann auf eine andere Spalte, dann in eine leere Spalte, dann in eine freie
Zelle. Karten, die nirgendwohin können, hüpfen auf der Stelle. Karten, die kein
späterer Zug mehr brauchen könnte, wandern von selbst auf die Ablage, und sobald
nichts mehr den Sieg verhindern kann, tun es alle. Ist Send cards home
ausgeschaltet, schickt Finish (`F`) sie dorthin.

Jeder Tipp stammt von einem Löser, der das Spiel vor Ihnen zu Ende spielt; ein
Tipp (`H`) ist daher ein Zug, der das Spiel gewinnbar hält, nicht bloß einer,
den die Regeln erlauben. Der Löser betrachtet jede Stellung, die Sie erreichen.
Beweist er, dass kein Gewinnweg mehr bleibt, sagt es die Statuszeile, und Back
to last winnable (`Ctrl+B`) bringt Sie zur letzten Stellung zurück, aus der das
Spiel noch zu gewinnen war.

Ist Winnable deals only eingeschaltet, wie zu Beginn, ist jedes neue Spiel
eines, das der Löser bereits gewonnen hat, und ein Häkchen neben der Spielnummer
zeigt es an. Der Löser gibt ein Spiel auf, das er nicht rasch entscheiden kann,
und das Spiel, das Sie dann bekommen, ist als ungeprüft gekennzeichnet; Spider
mit vier Farben ist oft zu tief, um rechtzeitig entschieden zu werden. Jedes
Spiel trägt in der Statuszeile seine Nummer, und das eigene Menü des Tisches,
geöffnet mit der zweiten Taste, gibt ein Spiel nach seiner Nummer. Die Spiele
von FreeCell sind so nummeriert wie in anderen FreeCell-Spielen, sodass ein
nummeriertes Spiel überall dasselbe ist.

Klondike wird nach den üblichen Regeln gezählt, oder nach Vegas, wo jedes Spiel
52 $ kostet und jede abgelegte Karte 5 $ bringt, mit einem Durchgang durch den
Stapel beim Ziehen einer Karte und dreien beim Ziehen von drei, oder nach
kumuliertem Vegas, das die Gewinne von Spiel zu Spiel mitnimmt. Spider beginnt
mit 500 Punkten, verliert einen für jeden Zug und gewinnt 100 für jede abgelegte
Reihe. Für jedes Spiel hält die Statistik die gespielten und gewonnenen Spiele
fest, die laufende Serie von Siegen oder Niederlagen, die längste Siegesserie,
den schnellsten Sieg und den Sieg mit den wenigsten Zügen. Wer neu gibt, bevor
ein Spiel gewonnen ist, zählt es als verloren.

Das laufende Spiel wird aufbewahrt: Schließen Sie das Fenster oder beenden Sie
das Programm, geht es beim nächsten Mal weiter, und die Uhr steht still, solange
das Fenster verborgen ist. Das Menü des Spiels in der Symbolleiste und das
eigene Menü des Tisches enthalten die Einstellungen: gewinnbare Spiele, die
Zählweise, das Ablegen von Karten, vierfarbige Farben, die Kartenrückseiten und
den Tisch.

Die Tastatur kann alles, was der Zeiger kann. Die Pfeiltasten bewegen einen Ring
von Stapel zu Stapel und an den Karten einer Spalte hinauf; `Space` nimmt die
Karten auf und legt sie dort ab, wo der Ring steht, `Enter` schickt sie an ihren
besten Platz und `Escape` legt sie zurück. `D` dreht den Stapel um, `Ctrl+Z`
nimmt einen Zug zurück und `Ctrl+Y` macht ihn erneut, `N` gibt ein neues Spiel,
`R` gibt dieses neu und `S` zeigt die Statistik.

Das Spiel wird aus der Programmbibliothek des Schreibtisches unter Games
gestartet oder in einer Shell über seinen Namen. Es benötigt eine laufende
grafische Sitzung: Ohne sie ist der Fensterkanal nicht erreichbar, und das Spiel
meldet die Ablehnung auf der Standardfehlerausgabe und beendet sich.

## EXIT STATUS

Null nach einem sauberen Beenden; ungleich null, wenn der Fensterkanal oder der
gemeinsame Bildspeicherbereich verweigert wurde (der Grund steht auf der
Standardfehlerausgabe).
