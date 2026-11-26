## NAME

gpucheck — jede bereitgestellte GPU prüfen, den Bildschirm, der zeigt,
was eine davon zeichnet, und einen eigenen Compute-Kernel

## SYNOPSIS

`gpucheck`

`gpucheck --present`

`gpucheck --shader FILE [--words N]`

## DESCRIPTION

Führt dieselbe kurze Arbeit auf jeder GPU aus, die ein Treiber
bereitstellt, sowie auf dem Software-Gerät des Systems, und gibt für
jede GPU an, ob beide übereinstimmen.

Die Arbeit schreibt ein bekanntes Muster in Speicher, den der Befehl
lesen kann, füllt den Speicher daneben, kopiert beides durch Speicher,
den die GPU für sich selbst behält, und wieder zurück, und verlangt
dann eine Füllung über das Ende eines Puffers hinaus, die jedes Gerät
ablehnen muss. Danach führt sie jedes der eigenen Grafikprogramme des
Systems auf einem bekannten Bild aus. Kann die GPU jede Art von Bild
aufnehmen, die der nächste Schritt verwendet, so kopiert die Arbeit
Bilder in den Speicher und aus ihm heraus sowie untereinander, löscht
sie und führt ein Programm aus, das Bilder liest, schreibt und
abtastet. Kann die GPU zeichnen, so zeichnet die Arbeit Formen, Punkte
und eine Linie in bekannten Farben in Bilder, wobei sie verdeckt, was
dahinter liegt, abgewandte Formen weglässt, Farben mischt und einige
unberührt lässt, und fasst vier Abtastwerte jedes Pixels zu einem
zusammen. Außerdem zeichnet sie anhand einer Liste im Speicher, die die
erste zu zeichnende Kopie einer Form nennt, worauf eine GPU, die das
nicht kann, antworten muss, indem sie das Zeichnen aufgibt. Anschließend
verlangt sie von der GPU, ein Programm über mehr Gruppen von Arbeit
auszuführen, als sie zulässt, worauf jedes Gerät antworten muss, indem
es diese Arbeit aufgibt. Zuletzt führt sie kleine Programme aus, die
zusammen jede Instruktion verwenden, die einem GPU-Programm erlaubt ist,
wobei jede Antwort so genau mit der des Software-Geräts übereinstimmen
muss, wie es die Definition ihrer Instruktion verlangt; eine GPU, die
eines davon nicht ausführen kann, wird daran nicht gemessen. Eine GPU
stimmt überein, wenn sie dieselben Bytes hinterlässt wie das
Software-Gerät, diese Füllung aus demselben Grund ablehnt, dieselbe
Arbeit aufgibt und auf jedes dieser Programme gleich antwortet.

Der Befehl gibt für jede GPU eine Zeile aus, die ihren Hardwareknoten
nennt: entweder dass die GPU übereinstimmt, oder den ersten gefundenen
Unterschied — die abgelehnte Arbeit, und bei welchem Schritt; ein Byte,
das anders gelesen wird; eine andere Antwort auf die Füllung; einen
Wert, den ein Grafikprogramm anders hinterlassen hat; ein Byte eines
Bildes, das anders gelesen wird; ein Byte einer Zeichnung, das anders
gelesen wird; eine andere Antwort auf das Zeichnen anhand einer Liste;
eine andere Antwort auf die Arbeit über die Grenzen der GPU hinaus; oder
eine Instruktion, deren Antwort in einem der kleinen Programme weiter
von der des Software-Geräts abweicht, als ihre Definition zulässt.

`sysinfo gpu` listet die GPUs auf, die der Befehl prüft.

Mit `--present` zeigt der Befehl stattdessen ein bekanntes Bild: einen
blauen Hintergrund, einen Farbstreifen, eine Reihe karierter Kacheln und
eine in die gegenüberliegende Ecke kopierte Kachel, in einem Fenster,
das den ganzen Bildschirm ausfüllt. Das Bild wird von der ersten GPU
gezeichnet, die ein Treiber bereitstellt, oder, wenn keine
bereitgestellt wird, vom Software-Gerät. Der Befehl präsentiert es
einmal pro Frame, bis Escape gedrückt oder das Fenster geschlossen wird,
und gibt dann aus, wie viele Frames er präsentiert hat, womit sie
gezeichnet wurden und ob der Bildschirm sie alle gezeigt hat.

Mit `--shader` führt der Befehl stattdessen ein eigenes Programm aus:
den Compute-Kernel in FILE, in WGSL geschrieben oder zu SPIR-V
kompiliert, unterschieden daran, wie die Datei beginnt. WGSL wird in
einem getrennten Prozess kompiliert, der sonst nichts erreichen kann,
und jede der beiden Arten wird geprüft, bevor eine GPU sie erhält. Ein
abgelehnter Kernel wird nicht ausgeführt: Die Ablehnung wird ausgegeben,
bei WGSL mit der Zeile und Spalte, auf die sie sich bezieht. Der Kernel
muss genau einen Compute-Einsprungpunkt haben, der einen Storage-Puffer
in Gruppe 0, Bindung 0 verwendet. Der Puffer fasst N Wörter, 256, sofern
`--words` nichts anderes angibt, wobei Wort *i* anfangs *i* enthält, und
der Kernel wird einmal je Wort ausgeführt. Er läuft auf jeder GPU, die
ein Treiber bereitstellt, und auf dem Software-Gerät; daneben hält die
GPU im Speicher eines eigenständigen Programms eine Markierung, die nie
in den Puffer des Kernels gelangen darf. Für jede GPU gibt der Befehl
entweder aus, dass beide übereinstimmen — der Puffer wird identisch
gelesen, oder beide gaben einen Kernel auf, der sein Zeitlimit
überschritt, und bedienten danach weiter Anfragen —, oder den ersten
Unterschied: ein Wort, das anders gelesen wird; die im Puffer gefundene
Markierung; einen abgelehnten, aufgegebenen oder nie beantworteten
Kernel, den das Software-Gerät ausgeführt hat; oder eine GPU, die keine
Anfrage mehr bediente, nachdem sie ihn aufgegeben hatte.

## OPTIONS

- `--present` — das Bild im Vollbild anzeigen und prüfen, ob der
  Bildschirm alle präsentierten Frames gezeigt hat.
- `--shader FILE` — den Compute-Kernel in FILE, WGSL-Quelltext oder ein
  SPIR-V-Modul, auf jeder bereitgestellten GPU und auf dem
  Software-Gerät ausführen.
- `--words N` — mit `--shader`, wie viele Wörter der Puffer des Kernels
  fasst: von 1 bis 65536, ohne Angabe 256.
- `-h, -?` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `gpucheck` — jede bereitgestellte GPU prüfen.
- `gpucheck --present` — das Bild im Vollbild anzeigen; zum Beenden
  Escape drücken.
- `gpucheck --shader double.wgsl` — einen WGSL-Kernel über 256 Wörter
  auf jeder bereitgestellten GPU ausführen.
- `gpucheck --shader kernel.spv --words 4096` — einen kompilierten
  Kernel über 4096 Wörter ausführen.

## EXIT STATUS

- `0` — jede bereitgestellte GPU stimmte mit dem Software-Gerät überein;
  mit `--present` zeigte der Bildschirm alle präsentierten Frames; mit
  `--shader` stimmte jede bereitgestellte GPU beim Kernel mit dem
  Software-Gerät überein.
- `1` — eine GPU wich ab oder war nicht erreichbar, keine GPU wird
  bereitgestellt, die bereitgestellten GPUs konnten nicht aufgelistet
  werden, oder das Software-Gerät konnte die Arbeit nicht ausführen;
  mit `--present` wurde ein Frame nicht gezeigt, keiner präsentiert,
  oder das Fenster oder das Gerät lehnte einen Schritt ab; mit
  `--shader` konnte die Datei nicht gelesen werden, der Kernel wurde
  abgelehnt, oder eine GPU wich ab. Eine nicht verstandene Befehlszeile
  endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Kürzel
  wie `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
