## NAME

datetime — Datum, Uhrzeit und Zeitzone des Rechners

## SYNOPSIS

`datetime`

## DESCRIPTION

Öffnet das Fenster **Date & Time** (Datum & Uhrzeit): Es zeigt den Stand
der Uhr des Rechners und woher diese Zeit stammt, und es enthält die
Bedienelemente, mit denen sie gestellt wird. Erreicht wird es über das
Menü der Desktop-Uhr selbst: Klicken Sie mit der rechten Maustaste auf
die Uhr in der Symbolleiste und wählen Sie **Set Date & Time…** (Datum &
Uhrzeit einstellen…). Bevor sich das Fenster öffnet, fragt der Desktop
nach dem Kennwort eines Kontos, das diese Einstellungen ändern darf.

Oben im Fenster stehen die sekundengenaue Uhrzeit, das Datum und die
Zeitzone mit ihrer Abkürzung und ihrem Versatz gegenüber UTC, alles in
der Zeitzone des Rechners.

**Date and Time** (Datum und Uhrzeit) enthält die Uhr selbst.

- **Set date and time automatically** (Datum und Uhrzeit automatisch
  einstellen) hält die Uhr mithilfe von Zeitservern im Netzwerk genau.
  Darunter nennt das Fenster, woher die Zeit der Uhr stammt: aus dem
  Netzwerk gestellt, und wann; von Hand gestellt; beim Start des Rechners
  vom Uhrenbaustein der Platine gelesen; oder überhaupt nicht gestellt.
  Das Ein- oder Ausschalten des Schalters wirkt sofort.
- Solange der Schalter eingeschaltet ist, werden Kalender und Uhrzeit
  angezeigt, lassen sich aber nicht ändern. Schalten Sie ihn aus, um die
  Uhr von Hand zu stellen.
- Wählen Sie im Kalender einen Tag, geben Sie die Uhrzeit ein oder
  stellen Sie sie schrittweise ein, und drücken Sie dann **Set**. Die
  Pfeiltasten verschieben den gewählten Tag um einen Tag bzw. eine
  Woche, `Page Up` und `Page Down` um einen Monat und zusammen mit
  `Shift` um ein Jahr; die Schaltflächen neben Monat und Jahr blättern
  durch Monate und Jahre, ohne den Tag zu ändern. Im Uhrzeitfeld wechselt
  `Tab` oder `:` von der Stunde zur Minute und weiter zur Sekunde, und
  `Up` und `Down` zählen den Teil, in dem der Cursor steht, hoch oder
  herunter. Im Zwölf-Stunden-Format ist der letzte Teil **AM** oder
  **PM**: Klicken Sie darauf oder drücken Sie `A` oder `P`. `Enter`
  stellt die Uhr.
- Bis Sie einen Tag wählen oder die Uhrzeit ändern, laufen Kalender und
  Uhrzeit mit der Uhr mit. Danach halten sie Ihre Auswahl fest, bis Sie
  die Uhr stellen oder das Fenster schließen.

Datum und Uhrzeit, die Sie einstellen, gelten in der Zeitzone des
Rechners. Eine Uhrzeit, die beim Vorstellen der Uhren übersprungen wird,
wird abgelehnt, und das Fenster nennt, wann die Uhren umgestellt werden.
Bei einer Uhrzeit, die beim Zurückstellen der Uhren zweimal durchlaufen
wird, fragt das Fenster nach: Wählen Sie, ob Sie die vor oder die nach
der Umstellung meinen. Ein Datum vor der Entstehung dieses Systems oder
mehr als ein Jahrhundert nach seiner Entstehung wird erst gestellt, wenn
Sie es bestätigen, denn Zertifikate würden dann nicht als vertrauenswürdig
gelten, und Dateien sähen älter oder neuer aus, als sie sind.

Nach dem Stellen der Uhr wird der Uhrenbaustein der Platine auf dieselbe
Zeit gestellt, damit der Rechner sie über einen Neustart hinweg behält.
Hat der Rechner keinen Uhrenbaustein, weist das Fenster darauf hin, dass
die Zeit beim nächsten Neustart verloren geht, sofern die Netzwerkzeit
nicht eingeschaltet ist.

**Time Zone** (Zeitzone) nennt die Zeitzone, die der Rechner führt, sagt,
wann die Uhren dort das nächste Mal vor- oder zurückgestellt werden und
um wie viel, und bietet die Schaltfläche **Change…** an, die eine Liste
aller Orte öffnet, die das System kennt: Tippen Sie einen Teil des
Namens einer Stadt, einer Region oder einer Zeitzone ein, um sie
einzugrenzen, und wählen Sie dann mit `Enter` oder einem Doppelklick. Zu
jedem Ort wird die Uhrzeit angezeigt, die dort gerade gilt. Eine neue
Zeitzone gilt sofort.

Die Zeitzone des Rechners zeigt der Anmeldebildschirm, und sie gilt für
jedes Konto, das in **Settings** (Einstellungen) keine eigene Zeitzone
gewählt hat.

Darf das Konto, unter dem das Fenster läuft, eine Änderung nicht
vornehmen, wird sie abgelehnt, eine Meldung nennt den Grund, und das
Fenster zeigt, was tatsächlich gilt. `Escape` oder **Close** schließt das
Fenster.

## EXIT STATUS

Null, wenn das Fenster geschlossen wird, auch nach einer abgelehnten
Änderung. Nicht null, wenn das Fenster nicht geöffnet werden konnte oder
seine Verbindung zum Desktop verloren ging; der Grund steht auf dem
Standardfehlerstrom.

## SEE ALSO

`configure`
