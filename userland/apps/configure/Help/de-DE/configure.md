## NAME

configure — die Einstellungen des Rechners lesen und setzen

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

Listet, zeigt und setzt die Einstellungen des Rechners, abgelegt unter
`/System/Settings/Configuration/system.conf`. Ohne Operand wird jede
Einstellung mit ihrem aktuellen Wert aufgelistet; mit einem Schlüssel
allein wird dessen Wert angezeigt; mit einem oder mehreren Paaren
`<key> <value>` werden diese Einstellungen gemeinsam geändert.

Mehrere Paare sind eine einzige Änderung, nicht mehrere: Jedes Paar wird
geprüft, bevor irgendetwas geändert wird, und die Einstellungen werden
dann in einem Schritt gespeichert. Eine Befehlszeile, die einen
unbekannten Schlüssel, einen Wert außerhalb der Menge seines Schlüssels
oder denselben Schlüssel zweimal nennt, ändert überhaupt nichts, sodass
eine Gruppe von Einstellungen nie halb angewendet zurückbleiben kann.

Nur ein Administrator darf eine Einstellung ändern: Ein gewöhnliches
Konto liest die Einstellungen des Rechners, ändert aber keine. Das
System bringt eigene Einstellungen mit, die gelten, bis ein
Administrator eine davon ändert, und die erste Änderung behält alle
übrigen Einstellungen bei, die das System mitgebracht hat. Eine Änderung
wirkt, sobald der Teil des Systems, der sie liest, das nächste Mal
startet (`os.loginType` bei der nächsten Anmeldung, die
`cache.*`-Schalter beim nächsten Systemstart), sofern ihr Schlüssel
unten nichts anderes angibt.

Die Schlüsselmenge ist geschlossen: ein unbekannter Schlüssel oder ein
Wert außerhalb der Menge eines Schlüssels wird mit Angabe der gültigen
Auswahl abgelehnt und ändert nichts.

- `os.loginType` — `text` oder `graphical`: welchen Sitzungstyp der
  Anmeldedienst für einen authentifizierten Benutzer startet.
  `graphical` (die Vorgabe) startet nach der Authentifizierung direkt
  die Desktop-Sitzung und fällt auf die Textanmeldung zurück, wenn die
  Maschine keine ausführen kann; `text` startet die Shell des Kontos —
  der Desktop läuft nur aus einer grafischen Anmeldung.
- `cache.all` — `on` oder `off`: der Haupt-Caching-Schalter. `on` (die
  Vorgabe) lässt jede Cache-Klasse unten ihrer eigenen Einstellung
  folgen; `off` ist eine Obergrenze, die jeden Speicher-Cache
  unabhängig von den Einstellungen je Klasse deaktiviert.
- `cache.filesystem`, `cache.block`, `cache.transform`,
  `cache.semantic` — `auto` oder `off`: die Schalter je Klasse für die
  vier rückgewinnbaren Speicher-Caches (Dateisystem-, Vollplatten-
  Block-, entpackte-Cluster- und Anwendungsstart-Cache). `auto` (die
  Vorgabe) lässt den Speicherdruck-Manager die Klasse steuern; `off`
  deaktiviert sie ganz. Es gibt kein `on` je Klasse: eine Klasse kann
  nicht gezwungen werden, Speicherdruck zu ignorieren. Eine Klasse ist
  praktisch `off`, sobald `cache.all` auf `off` steht.

Jeder Cache ist ein rückgewinnbarer Beschleuniger, niemals die Quelle
der Wahrheit; das Abschalten eines oder aller macht die betroffene
Arbeit daher nur langsamer — es ändert niemals ein Ergebnis.

- `net.ipv4.enabled`, `net.ipv6.enabled` — `true` oder `false`: die
  stackweiten Schalter der Adressfamilien. Beide sind standardmäßig
  `true`. Eine deaktivierte Familie bindet keine Adressen, beantwortet
  keine Pakete und lehnt einen Socket dieser Familie mit einem
  typisierten Fehler ab — nie ein stilles Verwerfen.
- `net.ipv6.privacy` — `true` oder `false`: ob der Stack temporäre
  (Privacy-)IPv6-Adressen zusätzlich zur stabilen Adresse bildet.
  `false` (die Vorgabe) nutzt nur die stabile SLAAC-Adresse.
- `net.tcp.syncookies` — `auto` oder `always`: die Abwehr gegen
  SYN-Fluten. `auto` (die Vorgabe) hält eine begrenzte Halb-offen-
  Warteschlange und weicht bei Überlauf auf zustandslose Cookies aus;
  `always` beantwortet jede Verbindungsanfrage zustandslos. Es gibt
  kein `off` — eine ungeschützte Verbindungswarteschlange ist keine
  Einstellung.
- `net.tcp.keepalive` — `true` oder `false`: ob TCP-Verbindungen auf
  einer inaktiven Leitung Keepalive-Prüfungen senden. `false` (die
  Vorgabe) prüft nie und trennt eine inaktive Verbindung nie; `true`
  prüft einen inaktiven Gegenpart nach dem üblichen Intervall und
  trennt die Verbindung, wenn er nicht mehr antwortet.
- `net.tcp.ecn` — `true` oder `false`: ob TCP-Verbindungen Explicit
  Congestion Notification aushandeln. `false` (die Vorgabe) lässt
  Verbindungen Not-ECT; `true` bietet ECN im Handshake an und behandelt
  danach eine Überlastmarkierung als Signal zum Drosseln, statt einen
  Paketverlust zu erzwingen.
- `net.sockets.mem` — `auto` oder eine Bytegröße wie `64M`: der
  Speicher, den der Netzwerkstapel über alle Prinzipale hinweg in
  Socket-Zustand halten darf. `auto` (die Vorgabe) bemisst ihn am RAM der
  Maschine, damit ein großer Server nicht an eine auf einer kleinen
  gewählte Zahl gebunden ist; eine Größe überschreibt das für eine Last,
  die Sie besser kennen. Jeder Prinzipal darf ein Sechzehntel des
  wirksamen Budgets halten, sodass immer Platz für sechzehn bleibt.
  Bytes statt einer Socket-Anzahl, denn dieselbe Anzahl Sockets sind im
  Ruhezustand einige Kilobyte und vollständig gepuffert Megabyte: das
  Budget trägt viele stille Verbindungen oder weniger beschäftigte, je
  nach tatsächlicher Last.
- `time.servers` — `none` oder eine kommagetrennte Liste von
  Netzwerkzeitservern, jeder ein Hostname oder eine Adresse. `none` (die
  Vorgabe) nennt keinen eigenen Server: Die Uhr wird dann von dem Server
  gestellt, den die Adresszuteilung des Netzes nennt, sonst vom
  öffentlichen Pool `pool.ntp.org`. Hier genannte Server haben Vorrang vor
  beiden.
- `time.refresh` — `6h`, `12h`, `1d`, `2d` oder `7d`: wie viel Laufzeit
  zwischen erneuten Uhrabfragen vergeht, sobald die Zeit bekannt ist. `1d`
  ist die Vorgabe. Eine ungestellte, unplausible oder lange veraltete Uhr
  wird unabhängig davon sofort korrigiert, sobald das Netz es erlaubt.
- `time.network` — `on` (die Vorgabe) oder `off`: ob die Uhr überhaupt
  aus dem Netz gestellt wird. Bei `off` geht keine Zeitabfrage an einen
  Server, und die Uhr wird von Hand gestellt oder vom Uhrenbaustein der
  Platine, der bei jedem Start ohnehin gelesen wird. Eine Änderung gilt
  sofort; die Einstellung liegt auf dem Systemdatenträger, daher gilt zu
  Beginn eines Starts die Vorgabe, bis dieser erreicht ist.
- `time.zone` — `default` oder der Name einer Zeitzone wie
  `Europe/London`: die Zeitzone des Rechners, die der Anmeldebildschirm
  zeigt und die ein Konto verwendet, das keine eigene gewählt hat.
  `default` ist die Zone, die das System als seine eigene mitbringt; ein
  Name, den die Zonenliste des Systems nicht enthält, wird übergangen,
  als wäre er `default`. Eine Änderung gilt sofort.
- `input.mouse.debounce` — ganze Millisekunden, standardmäßig `25`, `0`
  deaktiviert, höchstens `100`: wie lange nach dem Loslassen einer Maustaste
  der nächste Druck derselben Taste als Kontaktprellen ignoriert wird, statt
  als neuer Klick zu gelten. Ein abgenutzter Schalter kann wenige
  Millisekunden nach dem Loslassen einen zweiten Druck melden, der als ein
  Klick gemeint war. Bei einer Maus, deren Schnellfeuermodus absichtlich
  Klickpaare sendet, `0` setzen.
- `audio.output`, `audio.input` — standardmäßig `auto`, oder der Ort eines
  Endpunkts, wie `audioctl` ihn anzeigt (sechzehn Hexadezimalziffern, ein
  Punkt und der Index des Endpunkts): die Ausgabe und die Eingabe, die
  dieser Rechner als Standard bevorzugt. Die eigene Wahl einer Sitzung geht
  vor; dies ist die Wahl vor jeder Anmeldung und auf einem Rechner ohne
  Desktop. `auto` ist das zuerst gefundene Gerät.
- `audio.level` — in Dezibel, standardmäßig `0dB`, oder eine Dämpfung wie
  `-12dB` oder `-6.5dB`, mit höchstens zwei Nachkommastellen: der Pegel,
  mit dem jede Ausgabe und Eingabe beginnt. Nie über `0dB`. Der Audiodienst
  übernimmt einen geänderten Wert beim nächsten Start; `audioctl` ändert
  den laufenden Rechner.

Ein Schlüssel, der nicht in der obigen Liste steht, wird in einem
zweiten Register gesucht: in den Netzwerkeinstellungen jeder
Schnittstelle, abgelegt unter `/System/Settings/Network/network.conf`.
Seine Schlüssel lauten `<interface>.<setting>`, wobei `<interface>` der
Alias, den ein Administrator einer Netzwerkschnittstelle gegeben hat
(`wan`, `lan0`), und `<setting>` einer von `kind`, `match.mac`,
`match.node`, `ipv4.method`, `ipv4.address`, `ipv4.gateway`,
`ipv6.method`, `ipv6.address`, `ipv6.gateway`, `dns.servers`, `mtu`,
`bond.members`, `bond.mode`, `bond.monitor-interval` und `bond.primary`
ist.

Das obige Register des Rechners wird immer zuerst durchsucht, sodass ein
Schnittstellen-Alias nie den Namen einer Rechnereinstellung übernehmen
kann.

Die Netzwerkeinstellungen haben keine Vorgaben, daher zeigt die
Auflistung nur die gesetzten Einstellungen, und die Anzeige einer nicht
gesetzten antwortet mit einer leeren Zeile, statt einen Wert zu
erfinden. Nur ein Administrator darf sie lesen oder ändern, denn sie
benennen die Hardware jeder Schnittstelle und die Adressen dieses
Rechners. Für jedes andere Konto zeigt die Auflistung die Einstellungen
des Rechners und vermerkt, dass die Netzwerkeinstellungen nicht gezeigt
werden.

Da dieses Register keine Vorgaben hat, wird eine Einstellung entfernt
statt zurückgesetzt, und entfernt wird sie mit einem **leeren Wert**:
`configure wan.ipv4.address ""`. Keine Einstellung einer Schnittstelle
akzeptiert einen leeren Wert, daher kann diese Schreibweise nie mit dem
Setzen eines Werts verwechselt werden. So kann ein einziger Befehl eine
Schnittstelle von einer statischen Adresse auf DHCP umstellen:
`configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
— eine Änderung, deren Hälften jede für sich keine stimmige
Konfiguration beschreiben. Eine Schnittstelle, deren letzte Einstellung
entfernt wird, ist überhaupt nicht mehr deklariert.

Die gesamte bearbeitete Konfiguration wird geprüft, bevor irgendetwas
gespeichert wird: Eine Änderung, die sie widersprüchlich hinterließe
(eine statische Methode ohne Adresse, ein Bond mit weniger als zwei
Mitgliedern), wird abgelehnt, und nichts wird gespeichert.

Das Ändern einer `net.*`-Einstellung oder einer Einstellung einer
Schnittstelle speichert sie und stellt sie dem laufenden Netzwerkstapel
zu, sodass sie sofort wirkt. Zugestellt werden nur die Schnittstellen,
die die Änderung tatsächlich betrifft. Nimmt der laufende Stapel eine
Zustellung nicht an — es läuft keiner, oder Ihr Konto darf das Netzwerk
nicht verwalten —, wird die Einstellung trotzdem gespeichert, und
`configure` sagt das; sie gilt dann ab dem nächsten Systemstart.

Zwei Grenzen werden gemeldet statt verborgen. Eine Schnittstelle, die
Sie aus der Konfiguration entfernt haben, läuft bis zum nächsten
Systemstart mit der Konfiguration weiter, die der Stapel zuletzt
erhalten hat, denn der Stapel hat keine Nachricht, die eine
Schnittstelle stilllegt. Eine Schnittstelle, die weder mit `match.mac`
noch mit `match.node` gespeichert wurde, kann nie an ein Gerät gebunden
werden; sie wird daher gespeichert und die Ablehnung genannt.

## OPTIONS

- `-h, -?` — die Kurzhilfe dieses Befehls anzeigen.

## EXAMPLES

- `configure` — jede Einstellung auflisten.
- `configure os.loginType` — den vorgegebenen Sitzungstyp anzeigen.
- `configure os.loginType graphical` — in die grafische Anmeldung
  starten.
- `configure cache.all off` — jeden Speicher-Cache systemweit
  deaktivieren.
- `configure cache.filesystem off` — nur den Dateisystem-Cache
  deaktivieren.
- `configure net.ipv6.enabled false` — IPv6 stackweit abschalten.
- `configure time.servers 0.example.test,1.example.test` — die
  Netzwerkzeitserver setzen, nach denen die Uhr synchronisiert wird.
- `configure time.zone Europe/London` — den Rechner auf Londoner Zeit
  stellen.
- `configure wan.ipv4.address` — die statische IPv4-Adresse anzeigen,
  die für die Schnittstelle namens `wan` konfiguriert ist.
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  der Schnittstelle `wan` eine statische IPv4-Adresse und ein
  Standard-Gateway geben.
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — `wan` von statischer Adressierung auf DHCP umstellen.
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — die rekursiven
  DNS-Server setzen, die auf `wan` verwendet werden.

## EXIT STATUS

- `0` — Auflistung, Wert, Kurzhilfe oder Änderung wurden abgeschlossen.
- `1` — die Einstellungen konnten nicht gelesen oder geändert werden
  (z. B. darf der Aufrufer die Einstellungen des Rechners nicht ändern
  oder seine Netzwerkeinstellungen nicht lesen) oder die Ausgabe konnte
  nicht zugestellt werden.
- `2` — die Befehlszeile wurde nicht verstanden, der Schlüssel ist
  unbekannt, der Wert liegt außerhalb der Menge des Schlüssels, oder die
  Änderung würde die Netzwerkkonfiguration widersprüchlich hinterlassen.

## ENVIRONMENT

- `LANG` — die bevorzugte Sprache der Kurzhilfe (ein BCP-47-Kennzeichen
  wie `fr-FR`).

## SEE ALSO

- `man`
