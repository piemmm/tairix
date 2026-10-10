## NAME

play — Tondateien abspielen

## SYNOPSIS

`play [option...] file...`

## DESCRIPTION

Spielt jede Datei der Reihe nach auf einer Ausgabe ab. Jede Datei wird in einem
abgeschotteten Prozess ohne jede Befugnis dekodiert, sodass eine feindselige
Datei höchstens ihre eigene Dekodierung beenden kann: Sie wird mit genanntem
Grund ausgelassen, und der Rest der Liste wird gespielt. Gelesen werden AU-, WAV-
und FLAC-Dateien, FLAC eigenständig oder in Ogg.

Aufeinanderfolgende Dateien mit gleicher Abtastrate, gleichem Sampleformat und
gleicher Kanalanordnung werden lückenlos in einem Datenstrom gespielt. Eine
Datei anderer Form wartet, bis das bereits Eingereihte abgespielt ist, und
öffnet dann einen eigenen Strom.

Ist die Standardeingabe ein Terminal, zeichnet `play` eine Vollbildoberfläche:
die laufende Datei, die Position, den Pegel und eine Anzeige je Kanal sowie die
Liste. Die Wiedergabe hängt nicht davon ab. In den Hintergrund geschickt, spielt
`play` weiter und gibt das Terminal frei; in den Vordergrund geholt, zeichnet es
sich neu. Ohne Oberfläche meldet es den Fortschritt in einer Zeile auf der
Standardfehlerausgabe eines Terminals.

Die Oberfläche kennt diese Tasten: Leertaste oder `p` hält an und spielt
weiter; die Pfeile links und rechts springen zehn Sekunden; `n` oder `>` geht
zur nächsten Datei; `b` oder `<` springt an den Anfang dieser Datei oder in
ihren ersten drei Sekunden zur vorigen; `+`, `=` oder Pfeil nach oben ist drei
Dezibel lauter, `-`, `_` oder Pfeil nach unten drei leiser; `q` oder Strg-C
beendet; Strg-Z hält das Programm an, nachdem der Strom pausiert wurde.

Der Pegel eines Stroms ist eine Dämpfung: Ein Strom kann nicht über die
Vollaussteuerung angehoben werden, daher gehen `--gain` und die Pegeltasten
nicht über 0 dB. Für mehr Lautstärke die Lautstärke der Ausgabe erhöhen.

Eine Zeitangabe lautet `[[HH:]MM:]SS[.fraction]` — `90`, `1:30`, `1:02:03`
oder `12.5`.

Auf der Standardinformation (fd 3) schreibt `play` einen `schema`-Datensatz für
jede gespielte Datei, einen `omission`-Datensatz für jede ausgelassene oder
vorzeitig beendete Datei und am Ende einen `summary`-Datensatz.

## OPTIONS

- `-q, --quiet` — keine Oberfläche und keine Fortschrittszeile.
- `-v, --verbose` — Format und Länge jeder Datei auf der Standardfehlerausgabe.
- `--ui, --no-ui` — die Oberfläche zeichnen oder nie; `--ui` ohne Terminal
  wird abgelehnt.
- `-d, --device <sink>` — die Ausgabe: `audio:sink/default`,
  `audio:sink/<id>` für diesen Start oder `audio:sink/<location>`, wo
  immer das Gerät ist, wie `--list-devices` sie nennt.
- `-g, --gain <dB>` — der Pegel des Stroms, 0 oder darunter, auf das Hundertstel.
- `-s, --start <time>` — jede Datei zu dieser Zeit beginnen.
- `-t, --duration <time>` — so viel von jeder Datei spielen.
- `-l, --loop[=N]` — die Liste insgesamt N-mal spielen, ohne N endlos.
- `--list-devices` — die Ausgaben dieser Sitzung je mit Kennung und Ort
  nennen und beenden.
- `-h, -?, --help` — die Kurzhilfe dieses Befehls anzeigen.
- `--version` — die Version anzeigen und beenden.

## EXAMPLES

- `play song.wav` — eine Datei spielen, mit Oberfläche auf einem Terminal.
- `play -q intro.au song.wav &` — eine Liste im Hintergrund spielen.
- `play -s 1:30 -t 20 song.wav` — zwanzig Sekunden ab anderthalb Minuten spielen.
- `play -l3 -g -6 loop.wav` — eine Datei dreimal spielen, sechs Dezibel leiser.
- `play --list-devices` — die Ausgaben ansehen.

## EXIT STATUS

- `0` — jede Datei wurde gespielt, oder die Wiedergabe wurde ohne Auslassung beendet.
- `1` — eine Datei konnte nicht gespielt werden, oder die Wiedergabe konnte nicht weitergehen.
- `2` — die Befehlszeile wurde nicht verstanden.

## ENVIRONMENT

- `TERM` — das Terminal, für das die Oberfläche zeichnet.
- `LANG` — die bevorzugte Sprache der Kurzhilfe (ein BCP-47-Tag wie `fr-FR`).

## SEE ALSO

- `man`
