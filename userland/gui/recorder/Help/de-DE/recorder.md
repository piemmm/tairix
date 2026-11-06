## NAME

recorder — den Bildschirm für die Arbeitsoberfläche aufzeichnen

## SYNOPSIS

`recorder SESSION`

## DESCRIPTION

Zeichnet den Bildschirm als Film auf, solange die Arbeitsoberfläche es
verlangt. Das Programm wird nicht von Hand gestartet: Die Arbeitsoberfläche
startet es, wenn eine Aufzeichnung beginnt, und es endet mit der
Aufzeichnung.

Das Programm, das es startet, nennt als `SESSION` die Sitzung der
Arbeitsoberfläche, für die es aufzeichnet.

Zum Aufzeichnen drücken Sie `Meta+Shift+5`, um die Aufnahmeleiste zu öffnen,
wählen den ganzen Bildschirm, ein Fenster oder einen Bereich und drücken
**Record** (Aufzeichnen). Ein Aufnahmekürzel, die Aufzeichnungsanzeige im
Benachrichtigungsbereich oder die unter **Options** (Optionen) gewählte Dauer
beendet sie.

Eine Aufzeichnung wird als `Screen Recording YYYY-MM-DD at HH.MM.SS.webm` im
Ordner Desktop oder in `Home/Videos` gespeichert, wie es **Options**
festlegt. Sie ist ein WebM-Film: das Bild VP8-Video mit bis zu dreißig
Bildern pro Sekunde, aufgenommen nur, wenn sich auf dem Bildschirm etwas
ändert, der Ton Opus.

**Sound** (Ton) unter **Options** legt fest, was die Aufzeichnung hört:
nichts, das Mikrofon, den eigenen Ton der Arbeitsoberfläche oder beides
gemischt. Die Aufzeichnung eines Administrators mit Ton beginnt sofort. Die
jedes anderen Kontos fragt zuerst nach dem Passwort eines Administrators,
und das Aufzeichnungsprogramm läuft dann für genau diese eine Aufzeichnung
als dieser Administrator; ein Abbruch zeichnet nichts auf, und eine
Mitteilung sagt es.

Wenn der Datenträger voll ist, endet die Aufzeichnung dort und bleibt
erhalten. Wenn das Aufzeichnungsprogramm versagt, bleibt das Aufgezeichnete
erhalten, und eine Mitteilung sagt, was geschehen ist.

## EXIT STATUS

Null, wenn die Aufzeichnung fertig wurde, auch eine, bei der der Datenträger
voll lief. Ungleich null, wenn sie nicht beginnen konnte oder auf andere
Weise endete; der Grund steht auf der Standard-Fehlerausgabe.

## SEE ALSO

`desktop`
