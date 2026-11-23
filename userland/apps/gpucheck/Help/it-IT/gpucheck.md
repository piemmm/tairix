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
memoria che la GPU tiene per sé e poi indietro, e infine chiede un
riempimento oltre la fine di un buffer, che ogni dispositivo deve
rifiutare. Una GPU concorda quando lascia gli stessi byte del
dispositivo software e rifiuta quel riempimento per lo stesso motivo.

Il comando stampa una riga per ogni GPU, che ne nomina il nodo hardware:
o che la GPU concorda, o la prima differenza trovata — un byte letto
diversamente, il lavoro rifiutato, o una risposta diversa al
riempimento.

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
