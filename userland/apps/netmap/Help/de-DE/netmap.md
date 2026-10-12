## NAME

netmap — Netzwerkkarte und Bestandsaufnahme

## SYNOPSIS

`netmap`

## DESCRIPTION

Öffnet ein Fenster, das die Netzwerke kartiert, mit denen dieser Rechner
verbunden ist: seine Netzwerkschnittstellen, die Subnetze, in denen sie
liegen, die Hosts, die sein Nachbarcache auf jedem gesehen hat, die Router,
über die seine Routen führen, und das Standard-Gateway, das über das lokale
Netz hinausführt. Jedes Element wird als Symbol auf einer Scheibe in der Farbe
seiner Art gezeichnet — Rechner blau, Router orange, Drucker violett, Speicher
türkis, Telefone grün —, und jede Linie trägt die Farbe dessen, wohin sie
führt. Ein Host, dessen Betriebssystem die Karte erkennt, trägt dessen Emblem,
und eine Leuchte neben einem Host oder einer Schnittstelle zeigt, ob er
zuletzt erreichbar war (grün) oder nicht (rot).

Ziehen Sie mit der Haupttaste, drehen Sie das Mausrad oder scrollen Sie mit
zwei Fingern auf einem Touchpad, um sich auf der Karte zu bewegen. Halten Sie
`Ctrl` und drehen Sie das Rad, oder ziehen Sie auf einem Touchpad oder
Touchscreen die Finger zusammen und auseinander, um um den Punkt unter dem
Zeiger zu zoomen; mit `Home` passt die ganze Karte wieder ins Fenster. Klicken
Sie auf einen Knoten, um zu sehen, was über ihn bekannt ist — seine Adressen,
seine Hardwareadresse und seinen Hersteller, seine Erreichbarkeit und welche
Art Gerät er zu sein scheint und was darauf läuft, samt wie sicher sich die
Karte ist und warum —, und klicken Sie auf eine leere Stelle, um die Details
wieder zu schließen. Namen erscheinen überall, wo Platz für sie ist.

Ein Host, der Dienste anbietet, trägt ihre Anzahl auf einem kleinen Abzeichen.
Lassen Sie den Zeiger auf einem Knoten ruhen, um seinen vollen Namen zu sehen,
und auf einem solchen Host, um seine Dienste genannt zu bekommen; klicken Sie
auf den Host, um seine Dienste um ihn herum aufzufächern, und klicken Sie
erneut darauf, klicken Sie anderswohin oder drücken Sie `Escape`, um sie
wieder einzuklappen. Klicken Sie mit der Nebentaste auf einen Knoten, um sein
Menü zu öffnen: seine Dienste zeigen oder ausblenden oder seine Adresse,
seinen Namen oder seine Hardwareadresse kopieren — bei einem Dienst seine
Adresse mit Port, bei einem Webdienst seine URL.

Die Karte wird über die Systeminformations-API und den Erkennungsdienst
gelesen, und ihre Hosts werden von den eingestellten DNS-Servern benannt oder
mit dem Namen, den sich die Geräte selbst geben; sie kontaktiert nie selbst
einen Host und wird alle paar Sekunden neu gelesen, solange ihr Fenster offen
ist. Außerdem sammelt sie die Dienste, die Geräte im lokalen Netz anbieten —
Drucker, Dateifreigaben, Mediaplayer —, und ordnet jeden dem Host zu, der ihn
anbietet. Schnittstellenadressen und Routen erfordern `CAP_SYSINFO_GLOBAL`;
Hardwareadressen und der Nachbarcache erfordern `CAP_SYSINFO_HW`; angebotene
Dienste erfordern `CAP_NET_DISCOVER_ALL`; das Benennen von Hosts per DNS
erfordert `CAP_NET`. Fehlt eine davon, sagt die Karte, was sie nicht zeigen
kann, und zeichnet den Rest.

Netmap wird aus der Programmbibliothek des Desktops oder per Name aus einer
Shell gestartet und benötigt eine laufende grafische Sitzung; ohne sie meldet es
die Ablehnung auf dem Standardfehlerstrom und beendet sich.

## EXIT STATUS

Null nach Beenden; ungleich null, wenn der Fensterkanal oder der gemeinsame
Bildbereich verweigert wurde (der Grund steht auf dem Standardfehlerstrom).
