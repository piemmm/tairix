## NAME

datetime — data, ora e fuso orario della macchina

## SYNOPSIS

`datetime`

## DESCRIPTION

Apre la finestra **Date & Time** (Data e ora): ciò che segna l'orologio
della macchina, da dove proviene quell'ora e i controlli che lo
impostano. Vi si arriva dal menu dell'orologio del desktop — fare clic
con il tasto destro sull'orologio nella barra delle icone e scegliere
**Set Date & Time…** (Imposta data e ora…) — e, prima che la finestra si
apra, il desktop chiede la password di un account autorizzato a
modificare queste impostazioni.

La parte superiore della finestra mostra l'ora con precisione al
secondo, la data e il fuso orario con la sua abbreviazione e il suo
scostamento da UTC, il tutto nel fuso orario della macchina stessa.

La sezione **Date and Time** (Data e ora) contiene l'orologio vero e
proprio.

- L'interruttore **Set date and time automatically** (Imposta data e ora
  automaticamente) mantiene esatto l'orologio grazie ai server dell'ora
  presenti in rete. Sotto di esso la finestra indica da dove proviene
  l'ora dell'orologio: impostata dalla rete, e quando; impostata a mano;
  letta dal chip dell'orologio della scheda all'avvio della macchina;
  oppure non impostata affatto. Attivare o disattivare l'interruttore ha
  effetto immediato.
- Mentre l'interruttore è attivo, il calendario e l'ora sono mostrati ma
  non si possono modificare. Per impostare l'orologio a mano,
  disattivarlo.
- Scegliere un giorno nel calendario e digitare l'ora o regolarla passo
  passo, quindi premere **Set** (Imposta). I tasti freccia spostano il
  giorno scelto di un giorno o di una settimana, `Page Up` e `Page Down`
  di un mese e, con `Shift`, di un anno; i pulsanti accanto al mese e
  all'anno passano da un mese o da un anno all'altro senza cambiare il
  giorno. Nel campo dell'ora, `Tab` o `:` passa dalle ore ai minuti e
  poi ai secondi, e `Up` e `Down` regolano passo passo la parte in cui
  si trova il cursore. Con l'orologio a dodici ore l'ultima parte è
  **AM** o **PM**: fare clic su di essa oppure premere `A` o `P`.
  `Enter` imposta l'orologio.
- Finché non si sceglie un giorno o non si cambia l'ora, il calendario e
  l'ora seguono l'orologio man mano che avanza. Una volta fatto,
  mantengono quanto scelto finché non lo si imposta o non si chiude la
  finestra.

La data e l'ora impostate si intendono nel fuso orario della macchina.
Un'ora che gli orologi saltano quando vanno avanti viene rifiutata, e la
finestra indica quando cambia l'ora. Un'ora che gli orologi attraversano
due volte quando tornano indietro dà luogo a una domanda: scegliere se
si intende quella prima del cambio o quella dopo. Una data anteriore
alla creazione di questo sistema, o successiva di oltre un secolo, viene
impostata solo dopo una conferma, perché i certificati non sarebbero
ritenuti attendibili e i file sembrerebbero più vecchi, o più recenti,
di quanto non siano.

Dopo l'impostazione dell'orologio, il chip dell'orologio della scheda
viene portato alla stessa ora, così la macchina la conserva al riavvio.
Se la macchina non ha un chip dell'orologio, la finestra indica che
l'ora andrà persa al prossimo riavvio, a meno che l'ora di rete non sia
attiva.

La sezione **Time Zone** (Fuso orario) indica il fuso orario adottato
dalla macchina, dice quando i suoi orologi andranno avanti o indietro la
prossima volta e di quanto, e offre **Change…** (Cambia…), che apre un
elenco di tutti i luoghi noti al sistema: digitare parte del nome di una
città, di una regione o di un fuso per restringerlo, quindi scegliere
con `Enter` o con un doppio clic. Ogni luogo mostra l'ora attuale del
posto. Un nuovo fuso ha effetto immediato.

Il fuso della macchina è quello mostrato dalla schermata di accesso e
quello usato da ogni account che non abbia scelto un fuso proprio in
**Settings** (Impostazioni).

Se l'account con cui è in esecuzione la finestra non può apportare una
modifica, la modifica viene rifiutata, un avviso ne spiega il motivo e
la finestra mostra ciò che è davvero in vigore. `Escape` o **Close**
(Chiudi) chiude la finestra.

## EXIT STATUS

Zero quando la finestra viene chiusa, anche dopo una modifica rifiutata.
Diverso da zero quando la finestra non ha potuto essere aperta o la sua
connessione con il desktop è andata persa; il motivo è dichiarato sul
flusso di errore standard.

## SEE ALSO

`configure`
