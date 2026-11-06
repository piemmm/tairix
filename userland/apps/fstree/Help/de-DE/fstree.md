## NAME

fstree — der Vollbild-Dateimanager mit Verzeichnisbaum

## SYNOPSIS

`fstree [directory]`

## DESCRIPTION

Durchsucht das Dateisystem in einer tastaturgesteuerten Vollbildsitzung
nach dem Vorbild von XTree Gold: oben eine Kopfzeile mit
Datenträgerangaben, darunter ein Verzeichnisbaum-Fenster über einem
Dateifenster, das die Einträge des hervorgehobenen Verzeichnisses mit
Größe und Änderungszeit auflistet. Die Sitzung beginnt in `directory`
(ohne Angabe in der Wurzelansicht `/`).

Das Baumfenster ist das führende: Der Hervorhebungsbalken bewegt sich
darin, und das Dateifenster zeigt stets das hervorgehobene Verzeichnis.
`Enter` (oder `Tab`) wechselt in das Dateifenster, um Dateien zu
bearbeiten; `Esc` (oder `Tab`) kehrt zum Baum zurück — die Navigation
beginnt immer im Baum.

Der Baum wird verzögert gelesen: Der Inhalt eines Verzeichnisses wird
erst geholt, wenn es zum ersten Mal angezeigt oder aufgeklappt wird —
das Durchstöbern eines riesigen Datenträgers kostet also nur die
tatsächlich geöffneten Verzeichnisse. Ein Verzeichnis, das der Aufrufer
nicht auflisten darf, wird an Ort und Stelle verweigert: Der Fehler
erscheint in der Meldungszeile, die vorherige Ansicht bleibt erhalten;
nichts wird erfunden.

Tasten:

- `Up`/`Down` oder `k`/`j` — die Hervorhebung des aktiven Fensters
  bewegen; `PageUp`/`PageDown` bewegen sie um eine Bildschirmseite,
  `Home`/`End` springen zur ersten bzw. letzten Zeile. Bewegt sich die
  Baum-Hervorhebung, wird das neu hervorgehobene Verzeichnis im
  Dateifenster aufgelistet.
- `Right`/`l` — im Baum den hervorgehobenen Zweig aufklappen (dabei
  verzögert gelesen); im Dateifenster in das hervorgehobene Verzeichnis
  hinabsteigen.
- `Left`/`h` — im Baum den hervorgehobenen Zweig zuklappen oder — wenn
  er schon zu ist oder keine Unterverzeichnisse hat — die Hervorhebung
  zum übergeordneten Verzeichnis hinaufbewegen; im Dateifenster zum Baum
  zurückkehren.
- `+`/`-` — den hervorgehobenen Zweig des Baums auf- bzw. zuklappen.
- `Enter` — im Baum in das Dateifenster wechseln; im Dateifenster in das
  hervorgehobene Verzeichnis hinabsteigen (beide Fenster folgen) oder die
  hervorgehobene Datei in einem Vollbild-Betrachter öffnen: im
  Disassembler-Betrachter bei einer erkannten ausführbaren Datei, im
  Text-Pager, wenn sich die ersten Bytes der Datei als Text lesen, sonst
  im Hexdump (die Betrachter sind weiter unten beschrieben).
- `Esc` — einen laufenden Verbrauchslauf abbrechen (seine Zahlen bleiben
  erhalten); sonst aus dem Dateifenster zum Baumfenster zurückkehren.
- `o` — die hervorgehobene Datei in einem Betrachter Ihrer Wahl öffnen:
  `t` Text, `x` Hex oder `d` Disassembler.
- `Tab` — zwischen Baum- und Dateifenster wechseln.
- `s` — das Sortiermenü öffnen: `n` Name, `e` Erweiterung, `s` Größe,
  `m` Änderungszeit, `r` Richtung umkehren, `Esc` bricht ab.
  Verzeichnisse stehen stets vor den Dateien.
- `c` — den gewählten Eintrag kopieren: eine Eingabezeile fragt nach dem
  Ziel. Ein relatives Ziel landet im aufgelisteten Verzeichnis; ist das
  Ziel ein bestehendes Verzeichnis, wird die Kopie darin unter dem Namen
  der Quelle abgelegt. `Tab` vervollständigt den eingegebenen Pfad — ein
  eindeutiger Treffer wird eingesetzt (ein Verzeichnis bleibt mit seinem
  `/` offen), mehrere Treffer werden bis zu ihrem gemeinsamen Anfang
  ergänzt oder aufgelistet. Ein Verzeichnis wird mit allem darunter
  kopiert. Das Kopieren eines Eintrags auf sich selbst oder eines
  Verzeichnisses in seinen eigenen Teilbaum wird verweigert, bevor etwas
  geschrieben wird.
- `m` — den gewählten Eintrag verschieben, mit derselben Zielabfrage.
  Innerhalb eines Datenträgers ist das Verschieben ein atomares
  Umbenennen; über Datenträgergrenzen wird kopiert und danach die Quelle
  entfernt.
- `r` — den gewählten Eintrag an Ort und Stelle umbenennen: die
  Eingabezeile ist mit dem aktuellen Namen vorbelegt.
- `d` — den gewählten Eintrag nach einer Rückfrage löschen; nur `y`
  fährt fort. Das Löschen eines Verzeichnisses entfernt alles darunter,
  und die Rückfrage sagt das. Die Rückfrage lässt sich im
  Einstellungsmenü (`S`) abschalten.
- `M` — ein Verzeichnis im aufgelisteten Verzeichnis anlegen; der Name
  wird abgefragt.
- `a` — die Attribute des gewählten Eintrags öffnen: seine
  Berechtigungsbits und seine erweiterten Attribute (Paare
  `namespace.key = value`, z. B. `user.comment`). In der Ansicht bewegen
  die Pfeiltasten die Auswahl über die Attribute, `m` bearbeitet den Modus
  (eine oktale Eingabezeile, vorbelegt mit den aktuellen Bits; Enter
  wendet an — nur der Eigentümer des Eintrags darf ihn ändern, der Kernel
  weist alle anderen ab), `n` fügt ein Attribut als Zeile `key=value`
  hinzu, Enter bearbeitet das gewählte (vorbelegt; bei einem binären Wert
  wird nur der Schlüssel vorbelegt), `d` entfernt es, und Esc verlässt
  die Ansicht. Über jede Änderung entscheidet der Kernel — Lesen und
  Schreiben folgen den eigenen Berechtigungen des Eintrags, die
  Namensräume `system.`/`trusted.` sind reserviert, und eine Verweigerung
  wird wörtlich angezeigt. Ein Dateisystem, dessen Format keine Attribute
  speichert (FAT32, ext4), sagt das in der Ansicht; der Moduseditor
  funktioniert dort trotzdem.
- `t` — den gewählten Eintrag des Dateibereichs markieren oder die
  Markierung aufheben und eine Zeile hinabgehen; wiederholtes Drücken
  markiert also eine Reihe. Markierte Einträge tragen ein `*`.
- `T` — nach Muster markieren: ein Glob (`*`, `?`, `[...]`), der gegen
  die sichtbaren Namen geprüft wird, oder ein Bereich — `size:MIN..MAX`
  (Bytes, Suffixe `K`/`M`/`G`/`T`, jede Grenze darf offen bleiben:
  `size:1M..`, `size:..64K`) oder `date:YYYY-MM-DD..YYYY-MM-DD`
  (Änderungsdatum, Enddatum eingeschlossen; ein Eintrag, dessen
  Datenträgerformat keine Änderungszeit speichert, passt nie auf ein
  Datum). Jeder Treffer kommt zur Markierungsmenge hinzu; ein
  fehlerhaftes Muster oder ein fehlerhafter Bereich markiert nichts und
  nennt den Grund.
- `i` — die Markierungen über die sichtbaren Einträge invertieren.
- `C` — alle Markierungen aufheben.
- `f` — den Dateibereich mit einem Dateinamen-Glob filtern, der schon
  während der Eingabe angewandt wird. `Enter` behält den Filter (in der
  Statuszeile angezeigt), `Esc` stellt den Filter wieder her, wie er
  war. Ein Muster, das sich nicht kompilieren lässt, blendet nichts aus
  — die Statuszeile kennzeichnet es stattdessen mit `(bad pattern)`. Ein
  geleertes Muster hebt den Filter auf.
- `/` — den Zweig unter dem fokussierten Verzeichnis per
  Dateinamen-Glob durchsuchen, geprüft gegen den zweigrelativen Pfad
  jeder Datei. Die Ergebnisse erscheinen in der flachen Ansicht, sobald
  sie gefunden werden.
- `F` — Dateiinhalte nach einem wörtlichen Text durchsuchen, ohne
  Unterscheidung von Groß- und Kleinschreibung und in begrenzten
  Fenstern gestreamt (ein Treffer über eine Lesegrenze hinweg wird
  trotzdem gefunden). Sind Einträge markiert, deckt die Suche die
  Markierungsmenge ab (markierte Verzeichnisse rekursiv), sonst den
  fokussierten Zweig. Jede Ergebniszeile trägt ihre Trefferzahl; eine
  Datei, deren Inhalt binär aussieht, wird als binärer Treffer gemeldet
  — ihre Bytes werden nie angezeigt. Eine Datei, die sich nicht lesen
  lässt, wird im Bericht des Durchlaufs aufgeführt, nie stillschweigend
  übergangen.
- `u` — den Plattenverbrauch unter dem fokussierten Verzeichnis
  zählen: Dateien, Bytes und Verzeichnisse, schrittweise im
  Hintergrund durchlaufen. `Esc` bricht ab und behält die bis dahin
  gezählten Zahlen.
- `v` — den Zweig unter dem fokussierten Verzeichnis flach anzeigen:
  eine Liste jeder Datei darunter, seitenweise gefüllt (`Space` lädt die
  nächste Seite). In der Ansicht markieren `t`/`T`/`i`/`C` ihre Zeilen,
  `c`/`m`/`d` führen Stapeloperationen über die Markierungsmenge aus,
  `Enter` springt in den Bereichen zum Verzeichnis der gewählten Zeile
  (der Cursor landet auf ihrem Eintrag), und `Esc` kehrt zu den
  Bereichen zurück. Läuft ein Durchlauf oder eine Suche noch, hält `Esc`
  sie zuerst an und behält die bis dahin gefundenen Zeilen. Die Zeilen
  sind relativ zum flachen Zweig benannt. Suchergebnisse (`/`, `F`)
  füllen dieselbe Ansicht, sodass sich ihre Zeilen genau wie eine flache
  Auflistung markieren und bearbeiten lassen.
- `H` — versteckte Einträge (Punktnamen) in beiden Bereichen ein- und
  ausblenden.
- `.` — den letzten Dateivorgang auf die aktuelle Auswahl wiederholen:
  ein Kopieren oder Verschieben geht erneut in dasselbe Zielverzeichnis,
  ein Löschen fragt gemäß der Rückfrage-Einstellung erneut nach. Gab es
  noch keinen Vorgang, sagt die Taste das.
- `V` — die eingehängten Datenträger auflisten: jede Zeile zeigt den
  Einhängepunkt, den Dateisystemtyp und die freien/gesamten Bytes, sofern
  der Datenträger sie meldet. `Enter` setzt die Wurzel der ganzen Sitzung
  auf den gewählten Datenträger; `Esc` schließt die Liste. Werden keine
  Datenträger gemeldet, sagt die Taste das.
- `S` — das Einstellungsmenü: `1` schaltet die Rückfrage beim
  Einzellöschen um, `2` die beim Stapellöschen. Änderungen werden in den
  eigenen Einstellungen dieser Anwendung gespeichert, die nur sie lesen
  kann, und beim nächsten Start geladen; ist der Einstellungsdienst
  nicht erreichbar, gelten sie nur für die Sitzung, und das Menü nennt
  den Grund.
- `?` — diese Hilfe über den Bereichen anzeigen; jede Taste schließt sie.
- `q` — beenden und das Terminal wiederherstellen.

Solange Einträge markiert sind, wirken `c`, `m` und `d` auf die ganze
Markierungsmenge statt auf die Auswahl: `c`/`m` fragen nach einem
bestehenden Zielverzeichnis, in dem die Einträge landen (`Tab`
vervollständigt es), und `d` bestätigt das Stapellöschen (sofern diese
Rückfrage im Einstellungsmenü nicht abgeschaltet ist). Die Einträge
werden in Markierungsreihenfolge verarbeitet; ein fehlgeschlagener
Eintrag hält den Rest nie auf, der Abschlussbericht zählt das Gelungene,
und ein Berichtsbildschirm nennt jeden Fehlschlag beim Namen — ein Stapel
ist nie stillschweigend unvollständig. Gelungene Einträge verlieren ihre
Markierung; Fehlschläge bleiben für einen neuen Versuch markiert.

Würde ein Kopieren oder Verschieben eine bestehende Datei
überschreiben, fragt die Sitzung pro Datei: `o` überschreibt, `s`
überspringt (eine übersprungene Quelle bleibt an ihrem Platz), und `c`
bricht die verbleibenden Schritte ab — in einem Stapel verwirft der
Abbruch alle verbleibenden Einträge — bereits Angewandtes bleibt
bestehen, und der Abschlussbericht sagt, was geschah. Ein Fehler
mitten im Kopieren entfernt das halb geschriebene Ziel und zeigt den
Fehler des Kernels; nichts gibt sich je als vollständige Kopie aus.
Jede Operation wird vom Kernel autorisiert — eine Verweigerung
erscheint wörtlich in der Meldungszeile, ohne dass sich etwas ändert.
Eine Kopie behält die Lücken einer Datei und ihre erweiterten
Attribute; ein Attribut, das das Ziel nicht aufnehmen kann, wird nach
Abschluss des Vorgangs in der Meldungszeile genannt.

Ein Verzeichnis, das auf ein gleichnamiges Verzeichnis kopiert oder
verschoben wird, wird mit ihm zusammengeführt: Sein Inhalt kommt in das
vorhandene Verzeichnis, und nach jeder seiner Dateien, die dort schon
existiert, wird ebenso gefragt.

Die Kopfzeile mit den Datenträgerangaben oben zeigt den aufgelisteten
Pfad, die freien/gesamten Bytes des tragenden Datenträgers (sofern der
Systeminformationsdienst sie melden kann), die Zahl der sichtbaren
Einträge und — solange etwas markiert ist — die Zahl der Markierungen.
Die Statusleiste unter den Fenstern zeigt das aktive Fenster (Tree oder
Files), die Sortierordnung, ob versteckte Einträge angezeigt werden, den
aktiven Dateinamenfilter sowie die Zahl der Markierungen samt Bytesumme.
Eine Datei, deren Format keine Änderungszeit speichert, zeigt `-` in der
Zeitspalte.

Blendet der Dateibereich versteckte Einträge aus, vermerkt die Sitzung
das Auslassen zusätzlich auf dem Standard-Informationsstrom (fd 3) — ein
Hinweisdatensatz pro Änderung, abgreifbar mit `fstree 3>info.jsonl`; die
Sitzung selbst bleibt davon unberührt.

Die Betrachter: `Enter` auf einer regulären Datei öffnet sie
schreibgeschützt in einem Vollbild-Betrachter. Eine erkannte ausführbare
Datei — ein `rxe`-Abbild, ein 64-Bit-ELF, ein Wasm-Modul oder ein
eigenständiges signiertes Manifest — öffnet sich im
**Disassembler-Betrachter**; eine Datei, deren erste Bytes NUL-frei und
gültiges UTF-8 sind, im **Text-Pager**; alles andere im **Hexdump**. `o`
setzt sich über diese Wahl hinweg. `x` schaltet den offenen Betrachter an
derselben Stelle auf den Hexdump um, `t` auf den Text-Pager (auf den
Anfang der Zeile gerückt, die den angezeigten Offset enthält) und `d` auf
den Disassembler-Betrachter (eine Datei, die kein Containerformat für
sich beansprucht, fragt nach einem Befehlssatz und wird ab der aktuellen
Stelle als rohes Fragment dekodiert); `q` oder `Esc` kehrt zu den
Bereichen zurück.

Die Betrachter blättern die Datei in begrenzten Fenstern durch — die
Datei wird nie ganz im Speicher gehalten, sodass sich eine Datei jeder
Größe (weit über 4 GiB) korrekt blättern lässt — und teilen sich die
Tasten:

- `Up`/`Down` oder `k`/`j` — eine Zeile; `PageUp`/`PageDown`,
  `b`/`Space` — eine Seite; `Home`/`End` — der Anfang bzw. die letzte
  Seite.
- `g` — zu einer Stelle gehen: eine Zeilennummer ab 1 im Text-Pager,
  ein Byte-Offset (dezimal oder hexadezimal mit `0x`) im Hexdump, eine
  Adresse im Disassembler-Betrachter.
- `/` — ab der aktuellen Stelle vorwärts suchen; `n` wiederholt die
  letzte Suche hinter ihrem vorigen Treffer. Der Suchlauf läuft in
  begrenzten Schritten im Hintergrund — die Sitzung bleibt auch bei
  einer riesigen Datei bedienbar, die Statuszeile zeigt `searching…`,
  und `Esc` hält ihn an Ort und Stelle an.
- `?` — diese Hilfe; jede Taste schließt sie.

Der **Text-Pager** dekodiert UTF-8 mit einem sichtbaren Ersatzzeichen für
ungültige Bytes, expandiert Tabulatoren und zeigt jedes andere Steuerbyte
als sichtbares `·` — Dateiinhalte werden nie als rohe Escape-Sequenzen an
das Terminal gesendet. Lange Zeilen werden standardmäßig umbrochen; `w`
schaltet den Umbruch um (ohne Umbruch wird der Rest am rechten Rand
abgeschnitten). `/` sucht nach wörtlichem Text, ohne Unterscheidung von
Groß- und Kleinschreibung, über Lesegrenzen hinweg. Die Statuszeile zeigt
die Stelle als Zeilennummer; nach einem Sprung aus dem Hexdump ist die
Zeilennummer unbekannt (das Mitzählen wurde eingespart), bis ein Sprung
mit `g` sie neu verankert.

Der **Hexdump** zeigt das klassische Layout — eine Offset-Spalte,
sechzehn Bytes als Hex-Paare und eine ASCII-Spalte, in der nicht
druckbare Bytes als `.` erscheinen. Die Offset-Spalte ist so breit, wie
die Datei es verlangt (mindestens acht Hex-Ziffern). `/` sucht nach
wörtlichem Text (ohne Unterscheidung von Groß- und Kleinschreibung) oder,
geschrieben als `0x` gefolgt von Hex-Bytepaaren (`0xdeadbeef`), nach
einer exakten Bytefolge.

Der **Disassembler-Betrachter** öffnet mit einer Übersichtsseite: Format,
Befehlssatz und Einsprungpunkt des Containers, seine Regionen (Adresse,
Dateiabschnitt, Speichergröße, Berechtigungen) und seine Symbolzahl; ein
eigenständiges signiertes Manifest nennt stattdessen seine ABI-Version
und die Capabilities, die es anfordert. Ein `rxe`-Abbild trägt sein
Manifest neben sich, nie eingebettet, und die Übersicht sagt das.
`Up`/`Down` bewegen die Auswahl über die Regionszeilen, und `Enter`
öffnet die Disassemblierung der gewählten Code-Region (eine Datenregion
erscheint als Hex ihrer Dateibytes). Ein Container, der keinen
Befehlssatz nennt — ein `rxe`-Abbild läuft auf jeder Maschine, die es
lädt —, fragt einmal nach: `x` x86-64, `a` aarch64, `r` riscv64,
`w` wasm.

Der Codebereich zeigt einen Befehl pro Zeile — Adresse, Kodierungsbytes,
Mnemonic, Operanden — mit Label-Zeilen `<symbol>:` und symbolisierten
Sprungzielen (`<main+0x8>`), wo der Container Symbole nennt. Befehle
werden bildschirmweise dekodiert, nie die ganze Binärdatei im Voraus. `g`
springt zu einer Adresse, `/` durchsucht den Befehlstext, `End` läuft im
Hintergrund bis zum Ende der Region (`Esc` hält den Lauf an), und `I`
dekodiert mit einem anderen Befehlssatz neu. `Esc` kehrt zur
Übersichtsseite zurück.

Jede Container- und Befehlsdekodierung läuft in einem abgeschotteten
Hilfsprozess (der Parser-Sandbox), nie im Dateimanager selbst: Eine
bösartige ausführbare Datei kann den Hilfsprozess zum Absturz bringen,
nicht die Sitzung. Eine Datei, die der Decoder ablehnt — fehlerhaft oder
zu groß, um sie der Sandbox zu übergeben —, fällt mit einem einzeiligen
Hinweis auf den Hexdump zurück.

Ein Lesevorgang, den der Kernel mitten in der Anzeige verweigert,
schließt den Betrachter und zeigt den Fehler in der Meldungszeile —
veralteter Inhalt wird nie als aktuell ausgegeben.

## OPTIONS

- `directory` — das Startverzeichnis der Sitzung; Vorgabe ist die
  Wurzelansicht `/`.
- `-h`, `-?` — die Kurzform dieses Dokuments ausgeben und beenden.

## EXIT STATUS

- `0` — die Sitzung endete durch das `q` des Benutzers.
- `1` — das Startverzeichnis konnte nicht aufgelistet werden, oder der
  Terminalpfad schlug fehl. Eine nicht verstandene Befehlszeile endet
  ebenfalls mit `1`.

## SEE ALSO

ls, cp, mv, rm, mkdir, chmod, du, df, find
