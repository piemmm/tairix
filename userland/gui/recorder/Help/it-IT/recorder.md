## NAME

recorder — registrare lo schermo per la scrivania

## SYNOPSIS

`recorder SESSION`

## DESCRIPTION

Registra lo schermo in un filmato finché la scrivania glielo chiede. Non si
avvia a mano: la scrivania lo avvia quando inizia una registrazione, e
termina quando termina la registrazione.

Il programma che lo avvia indica come `SESSION` la sessione della scrivania
per cui registra.

Per registrare, premere `Meta+Shift+5` per aprire la barra di cattura,
scegliere l'intero schermo, una finestra o un'area, e premere **Record**
(Registra). Una scorciatoia di cattura, l'indicatore di registrazione
nell'area delle notifiche o la durata scelta in **Options** (Opzioni) la
interrompe.

Una registrazione viene salvata come
`Screen Recording YYYY-MM-DD at HH.MM.SS.webm` nella cartella Desktop o in
`Home/Videos`, come sceglie **Options**. È un filmato WebM: l'immagine è
video VP8 fino a trenta fotogrammi al secondo, ripresi solo quando qualcosa
cambia sullo schermo, e l'audio è Opus.

**Sound** (Audio) in **Options** sceglie che cosa sente la registrazione:
niente, il microfono, l'audio proprio della scrivania, o entrambi
mescolati. La registrazione con l'audio di un amministratore inizia
subito. Quella di ogni altro account chiede prima la password di un
amministratore, e il registratore funziona poi come quell'amministratore per
quell'unica registrazione; annullando non si registra nulla, e un avviso lo
dice.

Quando il disco si riempie, la registrazione finisce lì e viene conservata.
Quando il registratore non riesce, ciò che ha registrato viene conservato e
un avviso dice che cosa è successo.

## EXIT STATUS

Zero quando la registrazione è terminata, anche una interrotta dal disco
pieno. Diverso da zero quando non è potuta iniziare o è terminata in
qualsiasi altro modo; il motivo è scritto sull'errore standard.

## SEE ALSO

`desktop`
