## NAME

blendcheck — die Mischung der Anzeige-Engine und des Desktops vergleichen

## SYNOPSIS

`blendcheck`

## DESCRIPTION

Übernimmt den Bildschirm und zeigt über seine ganze Breite einen
Grauverlauf, darauf drei dunkle Glasscheiben, eine über der anderen:

1. vom Desktop gemischt, mit Dithering, so wie der Desktop
   durchscheinende Fenster zeichnet;
2. vom Desktop ohne das Dithering gemischt, sodass die Stufen
   sichtbar werden, die durch Streifenbildung in einem glatten
   Verlauf entstehen;
3. von der Anzeigehardware selbst gemischt.

Sieht die untere Scheibe aus wie die obere, mischt die Anzeigehardware
durchscheinende Flächen ohne Streifenbildung. Zeigt sie dagegen die
Stufen der mittleren, tut sie das nicht. Der Befehl gibt außerdem
aus, ob der Treiber der Anzeige erklärt, dass ihre Mischung
streifenfrei ist.

Drücken Sie eine beliebige Taste oder eine Taste des Zeigegeräts, um
den Bildschirm wieder freizugeben.

Der Befehl braucht die Anzeige für sich allein; führen Sie ihn daher in
einer Textsitzung aus und nicht in einem Terminal auf dem Desktop. Eine
Anzeige ohne Hardware-Ebenen hat keine eigene Mischung, die sich
vergleichen ließe, und der Befehl meldet das.

## OPTIONS

- `-h, -?` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `blendcheck` — den Vergleich anzeigen, bis eine Taste gedrückt wird.

## EXIT STATUS

- `0` — der Vergleich wurde angezeigt und geschlossen.
- `1` — die Anzeige war von einer Sitzung belegt, hat keine
  Hardware-Ebenen, hat das Testbild abgelehnt, oder der Arbeitsplatz
  ging verloren, während es angezeigt wurde. Eine nicht verstandene
  Befehlszeile endet ebenfalls mit `1`.

## ENVIRONMENT

- `LANG` — die bevorzugte Locale für die Kurzhilfe (ein BCP-47-Kürzel
  wie `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
