## NAME

wintersun — eine prozedural erzeugte Welt durchwandern

## SYNOPSIS

`wintersun [--seed SEED | --reference-scene]`

## DESCRIPTION

Öffnet ein Desktop-Fenster auf eine erzeugte Welt: die Draufsicht auf einen
Boden, den die Maschine synthetisiert statt ihn mitzuliefern, beleuchtet von
einer tief stehenden Sonne, die lange Schatten über jeden Hang wirft.

Die Welt reicht von einem Eisschild im hohen Norden bis zum Regenwald jenseits
des Äquators. Ihr Klima folgt der geographischen Breite, den vorherrschenden
Winden und den Gebirgen, die ihnen im Weg stehen: Eine Wüste liegt, wo der Regen
nicht hinkommt, ein Moor, wo das Wasser nicht abfließen kann. Jede Welt ist das
Ergebnis einer einzigen Zahl, ihres Startwerts: Derselbe Startwert öffnet auf
jeder Maschine dieselbe Welt.

Nichts an dieser Welt ist als Bildmaterial gespeichert. Jedes Material, aus dem
der Boden besteht — Eis, Dünensand, trockenes Gras, Waldboden, Granit — ist eine
Handvoll Zahlen, die der Client beim Zeichnen in eine Textur verwandelt. Die
Welt sieht deshalb auf jeder Maschine gleich aus und belegt fast keinen Platz
auf der Platte. Straßen tragen sich in das ein, was sie kreuzen, statt darauf zu
liegen.

Boden, der noch nicht erzeugt wurde, wird als die Lücke gezeichnet, die er
ist, und füllt sich, sobald er eintrifft. Der Client zeichnet, was er hat,
statt anzuhalten und zu warten — das Fenster bleibt also ansprechbar, während
die Welt aufholt.

Pfeiltasten oder `W`, `A`, `S`, `D` laufen. Zwei gleichzeitig gehaltene Tasten
laufen die Diagonale dazwischen mit derselben Geschwindigkeit, und
entgegengesetzte Tasten heben sich auf. Die Ansicht folgt Ihnen und hält am
Rand der Welt an, statt darüber hinauszugleiten. Zu steile Hänge und zu tiefes
Wasser lenken Sie ab.

`+` und `-` holen die Ansicht näher und weiter weg, in fünf Stufen von einer
Weltzelle über acht Pixel bis zu einer über hundertachtundzwanzig.

`F11` schaltet das Fenster in den Vollbildmodus und gibt es danach in seinen
vorherigen Zustand zurück: ein maximiertes Fenster kommt maximiert wieder.
`Esc` stellt es wieder her. `Q` beendet.

Jedes Detail wird in seiner feinsten Stufe gezeichnet, bis Sie etwas anderes
wählen. Die Zeile *Settings…* im Menü des Spiels in der Symbolleiste öffnet
sein Einstellungsfenster. Dort ist die Qualität *Ultra*, jedes Detail in
seiner feinsten Stufe; *Basic*, jedes Detail in seiner schlichtesten bei
voller Fenstergröße; *Custom*, Ihre eigene Wahl für Beleuchtung, Schatten,
Bodentextur und Rendergröße, jede auf ihrem eigenen Schieberegler; oder
*Auto*. Wer einen Schieberegler bewegt, macht die Wahl zu *Custom*. Ihre Wahl
bleibt für das nächste Spiel erhalten.

Unter *Auto* nimmt der Client Detail zurück, wenn die Bilder eine Weile zu
spät kamen — zuerst die Beleuchtung, dann die Schatten, dann die Rendergröße —
und gibt es Schritt für Schritt zurück, sobald sie sich erholen. Er urteilt
über Sekunden statt über einzelne Bilder, sodass ein Augenblick anderer Arbeit
auf der Maschine nichts kostet und ein größeres Fenster ihn nicht auf die
schlichteste Stufe schickt, und er zeichnet Figuren nie zu klein, um sie zu
erkennen.

Ein Fenster, das größer ist, als der Software-Renderer füllen kann, wird mit
höchstens 2560×1440 gezeichnet und auf die Fenstergröße hochskaliert.

## OPTIONS

- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.
- `--seed SEED, --seed=SEED` — die Welt öffnen, die SEED benennt, eine ganze
  Zahl von 0 bis 18446744073709551615. Ohne diese Option zieht das Spiel einen
  neuen Startwert und meldet ihn auf dem Standard-Informationsstrom, Deskriptor
  3, damit dieselbe Welt wieder geöffnet werden kann. Nicht zusammen mit
  `--reference-scene`, das eine feste Welt ist.
- `--reference-scene` — die feste Referenzszene zeichnen und stillhalten: eine
  Welt, dieselben Figuren und derselbe Augenblick, auf jeder Maschine gleich,
  damit ein Bild des Fensters mit einem anderswo gezeichneten verglichen
  werden kann. `F11` und `Esc` ändern weiterhin die Fenstergröße; sonst bewegt
  sich nichts.

## EXIT STATUS

`0`, wenn Sie beenden. Ein Status ungleich null nennt seinen Grund auf der
Standardfehlerausgabe: die Welt konnte nicht erzeugt werden, das Fenster
konnte nicht geöffnet werden, oder der Ereigniskanal der Sitzung ging
verloren.

- `2` — die Befehlszeile wurde nicht verstanden.
- `87` — die Referenzszene konnte nicht gezeichnet werden.

## SEE ALSO

`sapper`
