## NAME

audioctl — die Audiogeräte auflisten und ihre Regler ändern

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

Listet die Ausgaben und Eingaben, die diese Sitzung sieht: die Kennung jedes
Geräts für diesen Start, ob es der Standard seiner Richtung ist, seinen Pegel
und seine Stummschaltung, die Rate, mit der es läuft, die verlorenen Frames,
seinen Ort und seinen Namen. `streams` listet Ihre eigenen Datenströme, mit
`--all` die aller Prinzipale.

`default`, `level`, `mute` und `unmute` ändern die Regler eines Geräts. Ein
Gerät wird mit einem `audio:`-Verweis benannt: `audio:sink/default` oder
`audio:source/default` für den jetzigen Standard, `audio:sink/<id>` für diesen
Start oder `audio:sink/<location>`, wo immer das Gerät ist, wie die Liste sie
nennt. Ein Pegel ist in Dezibel auf das Hundertstel, 0 oder darunter, etwa
`-6` oder `-3.5`; ein negativer Pegel braucht kein `--`.

Die Regler eines Geräts gehören zu dem Raum, den es bedient. Die Sitzung, die
diesen Raum hält, darf sie ändern, jeder darf es, solange der Raum frei ist,
und niemand, solange er zurückgehalten wird; eine Ablehnung sagt das. Was
eine Sitzung einstellt, gehört ihr: Solange eine andere Sitzung den Raum hält,
treten ihre Einstellungen zurück, und sie gelten wieder, wenn sie zurückkehrt.
Der Anfangspegel jedes Geräts und die als Standard bevorzugten Geräte sind die
Maschineneinstellungen `audio.level`, `audio.output` und `audio.input`, die
`configure` setzt.

Auf der Standardinformation (fd 3) schreibt `audioctl streams` einen
`omission`-Datensatz, wenn es nur Ihre eigenen Datenströme listet.

## OPTIONS

- `-a, --all` — mit `streams` die Datenströme aller Prinzipale; dafür ist `CAP_SYSINFO_GLOBAL` nötig.
- `-h, -?, --help` — die eigene Kurzhilfe dieses Befehls anzeigen.
- `--version` — die Version anzeigen und beenden.

## EXAMPLES

- `audioctl` — die Ausgaben und Eingaben auflisten.
- `audioctl level audio:sink/default -10` — die Standardausgabe zehn Dezibel unter Vollaussteuerung stellen.
- `audioctl mute audio:source/default` — die Standardeingabe stummschalten.
- `audioctl default audio:sink/2` — Ausgabe 2 zum Standard machen.
- `audioctl streams --all` — die Datenströme aller Prinzipale auflisten.

## EXIT STATUS

- `0` — der Befehl wurde ausgeführt.
- `1` — er wurde abgelehnt oder konnte nicht ausgeführt werden.
- `2` — die Befehlszeile wurde nicht verstanden.

## ENVIRONMENT

- `LANG` — die bevorzugte Sprache für die Kurzhilfe (ein BCP-47-Tag wie `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
