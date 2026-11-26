## NAME

gpucheck — verificare ogni GPU servita, lo schermo che mostra ciò che
disegna una di esse, e un tuo kernel

## SYNOPSIS

`gpucheck`

`gpucheck --present`

`gpucheck --shader FILE [--words N]`

## DESCRIPTION

Esegue lo stesso breve lavoro su ogni GPU servita da un driver e sul
dispositivo software del sistema, e indica per ogni GPU se i due
concordano.

Il lavoro scrive uno schema noto in una memoria che il comando può
leggere, riempie la memoria accanto, copia entrambi attraverso una
memoria che la GPU tiene per sé e poi indietro, e in seguito chiede un
riempimento oltre la fine di un buffer, che ogni dispositivo deve
rifiutare. Poi esegue ciascuno dei programmi grafici propri del sistema
su un'immagine nota. Se la GPU può contenere ogni tipo di immagine che
il passo successivo usa, copia immagini da e verso la memoria e da
un'immagine all'altra, le cancella ed esegue un programma che legge,
scrive e campiona immagini. Se la GPU può disegnare, disegna in alcune
immagini forme, punti e una linea di colori noti, nascondendo ciò che
sta dietro, tralasciando le forme rivolte dall'altra parte, fondendo i
colori e lasciandone alcuni intatti, e riunisce quattro campioni di ogni
pixel in uno solo. Disegna anche a partire da un elenco in memoria che
nomina la prima copia di una forma da disegnare, e una GPU che non può
farlo deve rispondere abbandonando il disegno. Dopodiché chiede alla GPU
di eseguire un programma su più gruppi di lavoro di quanti ne consenta,
una richiesta a cui ogni dispositivo deve rispondere abbandonando quel
lavoro. Infine esegue piccoli programmi che nel loro insieme usano tutte
le istruzioni ammesse in un programma per GPU, e ogni risposta deve
corrispondere a quella del dispositivo software con la precisione
garantita dalla definizione della sua istruzione; una GPU che non può
eseguirne uno non viene valutata su di esso. Una GPU concorda quando
lascia gli stessi byte del dispositivo software, rifiuta quel
riempimento per lo stesso motivo, abbandona lo stesso lavoro e dà la
stessa risposta a ciascuno di quei programmi.

Il comando stampa una riga per ogni GPU, che ne nomina il nodo hardware:
o che la GPU concorda, o la prima differenza trovata — il lavoro
rifiutato, e a quale passo; un byte letto diversamente; una risposta
diversa al riempimento; un valore lasciato diversamente da un programma
grafico; un byte di un'immagine letto diversamente; un byte di un
disegno letto diversamente; una risposta diversa al disegno a partire da
un elenco; una risposta diversa al lavoro oltre i limiti della GPU; o
un'istruzione, in uno dei piccoli programmi, la cui risposta si discosta
da quella del dispositivo software più di quanto consenta la sua
definizione.

`sysinfo gpu` elenca le GPU che il comando verifica.

Con `--present` il comando mostra invece un'immagine nota: uno sfondo
blu, una fascia di colori, una fila di riquadri a scacchi e un riquadro
copiato nell'angolo opposto, in una finestra che copre tutto lo schermo.
L'immagine è disegnata dalla prima GPU servita da un driver, o dal
dispositivo software quando nessuna è servita. Il comando la presenta
una volta per fotogramma finché non si preme Esc o non si chiude la
finestra, poi stampa quanti fotogrammi ha presentato, che cosa li ha
disegnati e se lo schermo li ha mostrati tutti.

Con `--shader` il comando esegue invece un tuo programma: il kernel di
calcolo contenuto in FILE, scritto in WGSL o compilato in SPIR-V, due
forme distinte da come inizia il file. Il sorgente WGSL viene compilato
in un processo separato che non può raggiungere nient'altro, e in
entrambi i casi il kernel viene verificato prima di essere affidato a
una qualsiasi GPU. Un kernel rifiutato non viene eseguito: il rifiuto
viene stampato con la riga e la colonna a cui si riferisce nel sorgente
WGSL. Il kernel deve avere un solo punto d'ingresso di calcolo, che usa
un solo buffer di archiviazione nel gruppo 0, binding 0. Il buffer
contiene N parole, 256 se `--words` non indica altrimenti, con la
parola *i* che vale inizialmente *i*, e il kernel viene eseguito una
volta per ogni parola. Il comando lo esegue su ogni GPU servita da un
driver e sul dispositivo software; accanto al kernel, in una memoria che
appartiene a un programma a sé stante, la GPU tiene un marcatore che il
buffer del kernel non deve mai arrivare a contenere. Per ogni GPU il
comando stampa o che i due concordano — la lettura del buffer dà lo
stesso risultato, oppure entrambi hanno abbandonato un kernel che ha
superato il proprio limite di tempo e hanno continuato a servire — o la
prima differenza: una parola letta diversamente, il marcatore trovato
nel buffer, il kernel rifiutato, abbandonato o rimasto senza risposta
mentre il dispositivo software lo ha eseguito, o una GPU che, dopo
averlo abbandonato, non ha più servito nulla.

## OPTIONS

- `--present` — mostrare l'immagine a schermo intero e verificare che lo
  schermo abbia mostrato ogni fotogramma presentato.
- `--shader FILE` — eseguire il kernel di calcolo contenuto in FILE,
  sorgente WGSL o modulo SPIR-V, su ogni GPU servita e sul dispositivo
  software.
- `--words N` — con `--shader`, quante parole contiene il buffer del
  kernel: da 1 a 65536, e 256 se non è indicato.
- `-h, -?` — mostrare la guida breve di questo comando.

## EXAMPLES

- `gpucheck` — verificare ogni GPU servita.
- `gpucheck --present` — mostrare l'immagine a schermo intero; premere
  Esc per terminare.
- `gpucheck --shader double.wgsl` — eseguire su ogni GPU servita un
  kernel WGSL che opera su 256 parole.
- `gpucheck --shader kernel.spv --words 4096` — eseguire un kernel
  compilato che opera su 4096 parole.

## EXIT STATUS

- `0` — ogni GPU servita ha concordato con il dispositivo software; con
  `--present`, lo schermo ha mostrato ogni fotogramma presentato; con
  `--shader`, ogni GPU servita ha concordato con il dispositivo software
  quanto al kernel.
- `1` — una GPU è risultata diversa o non è stata raggiungibile, nessuna
  GPU è servita, le GPU servite non hanno potuto essere elencate, o il
  dispositivo software non ha potuto eseguire il lavoro; con
  `--present`, un fotogramma non è stato mostrato, nessuno è stato
  presentato, o la finestra o il dispositivo ha rifiutato un passo; con
  `--shader`, il file non ha potuto essere letto, il kernel è stato
  rifiutato, o una GPU è risultata diversa. Anche una riga di comando
  non compresa termina con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un'etichetta BCP-47
  come `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
