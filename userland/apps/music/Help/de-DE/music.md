## NAME

music — der Musikspieler des Desktops

## SYNOPSIS

`music`

## DESCRIPTION

Spielt eine Liste von Titeln in einem Fenster. Öffnen Sie Dateien oder einen
ganzen Ordner über die Dateiauswahl des Desktops oder öffnen Sie einen Titel
im Dateimanager: Er kommt in die Liste des schon laufenden Spielers. Titel mit
gleicher Rate, gleichem Sampleformat und gleicher Kanalanordnung gehen ohne
Lücke ineinander über.

Der Spieler hält keine Dateisystem-Berechtigung. Die Desktop-Sitzung sucht für
ihn und übergibt ihm, einmalig und nur lesbar, genau die Dateien, die der
Benutzer wählt — bei einem Ordner die darin liegenden Dateien, die dieser
Spieler öffnet. Keine Datei wird im Spieler dekodiert: Ton und Albumbild werden
je von einem eigenen Prozess ohne jede Reichweite dekodiert, sodass eine
fehlerhafte oder bösartige Datei nichts erreichen kann, was der Spieler
erreicht.

Oben im Fenster steht, was läuft: Albumbild, Titel, Künstler und Album, das
Format, die Stelle im Titel und eine Pegelanzeige je Kanal. Darunter liegen die
Transportsteuerung, Zufallswiedergabe und Wiederholung und die Lautstärke,
darunter die Liste. Ziehen Sie den Positionsregler zum Springen und den
Lautstärkeregler für den Pegel; beide wirken dort, wo Sie loslassen.
Doppelklicken Sie einen Titel, um ihn zu spielen. Ein Sekundärklick auf die
Liste öffnet das Menü des Spielers, das auch die Ausgabe wählt und ob Titel
nach der Lautheit angeglichen werden, die ihre eigenen Tags angeben.

Eine Datei, die der Spieler nicht lesen kann, wird ausgelassen, mit dem Grund
in der Statuszeile.

* `Space` — abspielen oder pausieren
* `Enter` — den gewählten Titel spielen
* `Left` / `Right` — zehn Sekunden zurück oder vor
* `Ctrl` + `Left` / `Right` — der vorige oder der nächste Titel
* `Up` / `Down` — den Titel darüber oder darunter wählen
* `Alt` + `Up` / `Down` — den gewählten Titel verschieben
* `Delete` — den gewählten Titel aus der Liste nehmen
* `+` / `-` — drei Dezibel lauter oder leiser
* `S` — zufällig oder der Reihe nach spielen
* `R` — nichts, die Liste oder den Titel wiederholen
* `Ctrl` + `O` — Dateien öffnen
* `Ctrl` + `Shift` + `O` — einen Ordner öffnen

## OPTIONS

`-h`, `-?`, `--help`
: Diese Hilfe auf die Standardausgabe schreiben und beenden.

## EXIT STATUS

Null, nachdem das Fenster geschlossen oder Beenden gewählt wurde. Ungleich
null, wenn der Fensterkanal, der gemeinsame Bildbereich oder die Desktop-Sitzung
abgelehnt wurde; der Grund steht auf der Standardfehlerausgabe.
