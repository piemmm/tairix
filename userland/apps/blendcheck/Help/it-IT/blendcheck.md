## NAME

blendcheck — confrontare la fusione del motore del display con quella del desktop

## SYNOPSIS

`blendcheck`

## DESCRIPTION

Prende il controllo dello schermo e mostra una rampa di grigi per tutta
la sua larghezza, a cui sovrappone tre lastre di vetro scuro, una sopra
l'altra:

1. fusa dal desktop, con dithering, come fa il desktop quando disegna le
   finestre traslucide;
2. fusa dal desktop senza dithering, il che mostra i gradini che il
   banding lascia in una sfumatura continua;
3. fusa dall'hardware del display stesso.

Se la lastra in basso appare come quella in alto, l'hardware del display
realizza la traslucenza senza banding. Se mostra i gradini di quella
centrale, non lo fa. Il comando stampa anche se il driver del display
dichiara che la propria fusione è priva di banding.

Premere un tasto qualsiasi o un pulsante del puntatore per restituire lo
schermo.

Il comando ha bisogno del display tutto per sé, quindi va eseguito da
una sessione testuale anziché da un terminale sul desktop. Un display
privo di livelli hardware non ha una fusione propria da confrontare, e
il comando lo segnala.

## OPTIONS

- `-h, -?` — mostrare la guida breve di questo comando.

## EXAMPLES

- `blendcheck` — mostrare il confronto finché non si preme un tasto.

## EXIT STATUS

- `0` — il confronto è stato mostrato e chiuso.
- `1` — il display era occupato da una sessione, non ha livelli
  hardware, ha rifiutato l'immagine di prova, o la postazione è andata
  persa durante il confronto. Anche una riga di comando non compresa
  termina con `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un'etichetta BCP-47
  come `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
