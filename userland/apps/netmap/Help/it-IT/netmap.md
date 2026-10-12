## NAME

netmap — mappa e inventario della rete

## SYNOPSIS

`netmap`

## DESCRIPTION

Apre una finestra che mappa le reti a cui questa macchina è collegata: le sue
interfacce di rete, le sottoreti in cui si trovano, gli host che la sua cache
dei vicini ha visto su ciascuna, i router attraverso cui passano le sue rotte
e il gateway predefinito che porta oltre la rete locale. Ogni elemento è
disegnato come un'icona su un disco del colore di ciò che è — computer in blu,
router in arancione, stampanti in viola, archiviazione in verde acqua,
telefoni in verde — e ogni linea prende il colore di ciò a cui porta. Un host
di cui la mappa riconosce il sistema operativo ne porta l'emblema, e una spia
accanto a un host o a un'interfaccia mostra se era raggiungibile l'ultima
volta che è stato visto (verde) o no (rosso).

Trascinate con il pulsante principale, girate la rotella o scorrete con due
dita su un touchpad per muovervi sulla mappa. Tenete premuto `Ctrl` e girate
la rotella, oppure pizzicate su un touchpad o uno schermo tattile, per
ingrandire attorno al punto sotto il puntatore; premete `Home` per far stare
di nuovo l'intera mappa nella finestra. Fate clic su un nodo per vedere che
cosa si sa di esso — i suoi indirizzi, l'indirizzo hardware e il suo
produttore, la raggiungibilità, il tipo di dispositivo che sembra essere e che
cosa esegue, con quanto la mappa ne è sicura e perché — e fate clic su uno
spazio vuoto per chiudere i dettagli. I nomi compaiono ovunque ci sia spazio
per loro.

La mappa viene letta tramite l'API di informazioni di sistema e il servizio di
rilevamento, e i suoi host ricevono il nome dai server DNS configurati o
quello che i dispositivi si danno da sé; non contatta mai direttamente un
host, e viene riletta ogni pochi secondi finché la sua finestra è aperta.
Accanto agli host mostra i servizi che i dispositivi annunciano sul
collegamento locale — stampanti, condivisioni di file, lettori multimediali —
ciascuno sotto l'host che lo offre. Gli indirizzi delle interfacce e le rotte
richiedono `CAP_SYSINFO_GLOBAL`; gli indirizzi hardware e la cache dei vicini
richiedono `CAP_SYSINFO_HW`; i servizi annunciati richiedono
`CAP_NET_DISCOVER_ALL`; dare un nome agli host tramite DNS richiede `CAP_NET`.
Senza una di esse, la mappa dice cosa non può mostrare e disegna il resto.

Netmap si avvia dalla Libreria dei programmi del desktop o per nome da una
shell, e richiede una sessione grafica attiva; senza di essa segnala il rifiuto
sul flusso di errore standard ed esce.

## EXIT STATUS

Zero dopo Esci; diverso da zero quando il canale della finestra o la regione
d'immagine condivisa è stato rifiutato (il motivo è indicato sul flusso di
errore standard).
