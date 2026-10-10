## NAME

play — riprodurre file sonori

## SYNOPSIS

`play [option...] file...`

## DESCRIPTION

Riproduce ogni file a turno su un'uscita. Ogni file viene decodificato in un
processo isolato privo di qualsiasi autorità, così che un file ostile può al
massimo porre fine alla propria decodifica: viene escluso indicandone il motivo
e il resto dell'elenco viene riprodotto. Si leggono file AU, WAV e FLAC,
quest'ultimo nativo o in Ogg.

I file consecutivi con stessa frequenza, stesso formato di campione e stessa
disposizione dei canali si susseguono senza pause, in un solo flusso. Un file
di forma diversa attende che quanto è in coda sia riprodotto, poi apre un
proprio flusso.

Quando l'ingresso standard è un terminale, `play` disegna un'interfaccia a
schermo intero: il file in riproduzione, la posizione, il livello e un
indicatore per canale, e l'elenco. La riproduzione non ne dipende. Mandato in
secondo piano, `play` continua a suonare e restituisce il terminale; riportato
in primo piano, si ridisegna. Senza interfaccia, riporta l'avanzamento in una
riga sull'uscita d'errore di un terminale.

L'interfaccia accetta questi tasti: Spazio o `p` mette in pausa e riprende; le
frecce sinistra e destra spostano di dieci secondi; `n` o `>` passa al file
successivo; `b` o `<` torna all'inizio di questo file, o al precedente nei suoi
primi tre secondi; `+`, `=` o la freccia su alza di tre decibel, `-`, `_` o la
freccia giù li abbassa; `q` o Ctrl-C ferma; Ctrl-Z sospende, dopo aver messo in
pausa il flusso.

Il livello di un flusso è un'attenuazione: un flusso non può superare il fondo
scala, quindi `--gain` e i tasti di livello non vanno oltre 0 dB. Per suonare
più forte, alzare il volume dell'uscita.

Un tempo si scrive `[[HH:]MM:]SS[.fraction]` — `90`, `1:30`, `1:02:03` o
`12.5`.

Sull'informazione standard (fd 3) `play` scrive un record `schema` per ogni
file che riproduce, un record `omission` per ogni file escluso o interrotto, e
un record `summary` al termine.

## OPTIONS

- `-q, --quiet` — nessuna interfaccia e nessuna riga di avanzamento.
- `-v, --verbose` — formato e durata di ogni file sull'uscita d'errore.
- `--ui, --no-ui` — disegnare l'interfaccia, o mai; `--ui` senza terminale è
  rifiutato.
- `-d, --device <sink>` — l'uscita: `audio:sink/default`,
  `audio:sink/<id>` per questo avvio, o `audio:sink/<location>` ovunque
  sia il dispositivo, come le nomina `--list-devices`.
- `-g, --gain <dB>` — il livello del flusso, 0 o meno, al centesimo.
- `-s, --start <time>` — iniziare ogni file a questo tempo.
- `-t, --duration <time>` — riprodurre questa durata di ogni file.
- `-l, --loop[=N]` — riprodurre l'elenco N volte in tutto, o senza N per sempre.
- `--list-devices` — elencare le uscite di questa sessione, ciascuna con il
  suo identificativo e la sua posizione, e uscire.
- `-h, -?, --help` — mostrare la guida breve di questo comando.
- `--version` — mostrare la versione e uscire.

## EXAMPLES

- `play song.wav` — riprodurre un file, con l'interfaccia su un terminale.
- `play -q intro.au song.wav &` — riprodurre un elenco in secondo piano.
- `play -s 1:30 -t 20 song.wav` — riprodurre venti secondi da un minuto e mezzo.
- `play -l3 -g -6 loop.wav` — riprodurre un file tre volte, sei decibel più basso.
- `play --list-devices` — vedere le uscite.

## EXIT STATUS

- `0` — ogni file è stato riprodotto, o la riproduzione è stata fermata senza esclusioni.
- `1` — un file non ha potuto essere riprodotto, o la riproduzione non ha potuto continuare.
- `2` — la riga di comando non è stata compresa.

## ENVIRONMENT

- `TERM` — il terminale per cui l'interfaccia disegna.
- `LANG` — la lingua preferita della guida breve (un'etichetta BCP-47 come
  `fr-FR`).

## SEE ALSO

- `man`
