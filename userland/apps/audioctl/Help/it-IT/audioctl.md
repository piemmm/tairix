## NAME

audioctl — elencare i dispositivi audio e cambiarne i controlli

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

Elenca le uscite e gli ingressi che questa sessione vede: l'identificativo di
ciascuno per questo avvio, se è il predefinito della sua direzione, il livello
e il silenziamento, la frequenza a cui funziona, i frame persi, la posizione e
il nome. `streams` elenca i propri flussi, e con `--all` quelli di tutti i
principali.

`default`, `level`, `mute` e `unmute` cambiano i controlli di un dispositivo.
Un dispositivo si indica con un riferimento `audio:`: `audio:sink/default` o
`audio:source/default` per il predefinito in quel momento, `audio:sink/<id>`
per questo avvio, oppure `audio:sink/<location>` ovunque sia il dispositivo,
come li nomina l'elenco. Un livello è in decibel al centesimo, 0 o meno, come
`-6` o `-3.5`; un livello negativo non richiede `--`.

I controlli di un dispositivo appartengono alla stanza che serve. La sessione
che occupa quella stanza può cambiarli, chiunque può finché la stanza è
libera, e nessuno finché è trattenuta; un rifiuto lo dice. Ciò che una
sessione imposta è suo: mentre un'altra sessione occupa la stanza si fa da
parte, e torna quando essa rientra. Il livello iniziale di ogni dispositivo e
i dispositivi preferiti come predefiniti sono le impostazioni della macchina
`audio.level`, `audio.output` e `audio.input`, che `configure` imposta.

Sull'informazione standard (fd 3) `audioctl streams` scrive un record
`omission` quando elenca solo i propri flussi.

## OPTIONS

- `-a, --all` — con `streams`, i flussi di tutti i principali; richiede `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — mostrare la guida breve di questo comando.
- `--version` — mostrare la versione ed uscire.

## EXAMPLES

- `audioctl` — elencare le uscite e gli ingressi.
- `audioctl level audio:sink/default -10` — portare l'uscita predefinita a dieci decibel sotto il massimo.
- `audioctl mute audio:source/default` — silenziare l'ingresso predefinito.
- `audioctl default audio:sink/2` — rendere predefinita l'uscita 2.
- `audioctl streams --all` — elencare i flussi di tutti i principali.

## EXIT STATUS

- `0` — il comando è stato completato.
- `1` — è stato rifiutato, o non è stato possibile eseguirlo.
- `2` — la riga di comando non è stata compresa.

## ENVIRONMENT

- `LANG` — la lingua preferita per la guida breve (un tag BCP-47 come `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
