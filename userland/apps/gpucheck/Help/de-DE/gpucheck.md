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
schließlich eine Füllung über das Ende eines Puffers hinaus, die jedes
Gerät ablehnen muss. Eine GPU stimmt überein, wenn sie dieselben Bytes
hinterlässt wie das Software-Gerät und diese Füllung aus demselben
Grund ablehnt.

Der Befehl gibt für jede GPU eine Zeile aus, die ihren Hardwareknoten
nennt: entweder dass die GPU übereinstimmt, oder den ersten gefundenen
Unterschied — ein Byte, das anders gelesen wird, die abgelehnte Arbeit
oder eine andere Antwort auf die Füllung.

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
