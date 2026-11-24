## NAME

gpucheck — jede bereitgestellte GPU mit dem Software-Gerät vergleichen

## SYNOPSIS

`gpucheck`

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
nicht kann, antworten muss, indem sie das Zeichnen aufgibt. Zuletzt
verlangt sie von der GPU, ein Programm über mehr Gruppen von Arbeit
auszuführen, als sie zulässt, worauf jedes Gerät antworten muss, indem
es diese Arbeit aufgibt. Eine GPU stimmt überein, wenn sie dieselben
Bytes hinterlässt wie das Software-Gerät, diese Füllung aus demselben
Grund ablehnt und dieselbe Arbeit aufgibt.

Der Befehl gibt für jede GPU eine Zeile aus, die ihren Hardwareknoten
nennt: entweder dass die GPU übereinstimmt, oder den ersten gefundenen
Unterschied — die abgelehnte Arbeit, und bei welchem Schritt; ein Byte,
das anders gelesen wird; eine andere Antwort auf die Füllung; einen
Wert, den ein Grafikprogramm anders hinterlassen hat; ein Byte eines
Bildes, das anders gelesen wird; ein Byte einer Zeichnung, das anders
gelesen wird; eine andere Antwort auf das Zeichnen anhand einer Liste;
oder eine andere Antwort auf die Arbeit über die Grenzen der GPU hinaus.

`sysinfo gpu` listet die GPUs auf, die der Befehl prüft.

## OPTIONS

- `-h, -?` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `gpucheck` — jede bereitgestellte GPU prüfen.

## EXIT STATUS

- `0` — jede bereitgestellte GPU stimmte mit dem Software-Gerät überein.
- `1` — eine GPU wich ab oder war nicht erreichbar, keine GPU wird
  bereitgestellt, die bereitgestellten GPUs konnten nicht aufgelistet
  werden, oder das Software-Gerät konnte die Arbeit nicht ausführen. Eine
  nicht verstandene Befehlszeile endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Kürzel
  wie `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
