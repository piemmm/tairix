## NAME

gpucheck — verificare ogni GPU servita rispetto al dispositivo software

## SYNOPSIS

`gpucheck`

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
farlo deve rispondere abbandonando il disegno. Infine chiede alla GPU di
eseguire un programma su più gruppi di lavoro di quanti ne consenta, una
richiesta a cui ogni dispositivo deve rispondere abbandonando quel
lavoro. Una GPU concorda quando lascia gli stessi byte del dispositivo
software, rifiuta quel riempimento per lo stesso motivo e abbandona lo
stesso lavoro.

Il comando stampa una riga per ogni GPU, che ne nomina il nodo hardware:
o che la GPU concorda, o la prima differenza trovata — il lavoro
rifiutato, e a quale passo; un byte letto diversamente; una risposta
diversa al riempimento; un valore lasciato diversamente da un programma
grafico; un byte di un'immagine letto diversamente; un byte di un
disegno letto diversamente; una risposta diversa al disegno a partire da
un elenco; o una risposta diversa al lavoro oltre i limiti della GPU.

`sysinfo gpu` elenca le GPU che il comando verifica.

## OPTIONS

- `-h, -?` — mostrare la guida breve di questo comando.

## EXAMPLES

- `gpucheck` — verificare ogni GPU servita.

## EXIT STATUS

- `0` — ogni GPU servita ha concordato con il dispositivo software.
- `1` — una GPU è risultata diversa o non è stata raggiungibile, nessuna
  GPU è servita, le GPU servite non hanno potuto essere elencate, o il
  dispositivo software non ha potuto eseguire il lavoro. Anche una riga di
  comando non compresa termina con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un'etichetta BCP-47
  come `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
