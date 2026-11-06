## NAME

fstree — il gestore di file ad albero a schermo intero

## SYNOPSIS

`fstree [directory]`

## DESCRIPTION

Esplora il file system in una sessione a schermo intero guidata dalla
tastiera, sul modello di XTree Gold: in alto un'intestazione con le
statistiche del disco, una finestra con l'albero delle directory sopra
una finestra dei file che elenca le voci della directory evidenziata con
dimensioni e data di modifica. La sessione parte da `directory` (la vista
radice `/` se omessa).

La finestra dell'albero è quella principale: la barra di evidenziazione
si muove al suo interno e la finestra dei file mostra sempre la directory
evidenziata. `Enter` (o `Tab`) entra nella finestra dei file per agire sui
file; `Esc` (o `Tab`) torna all'albero — la navigazione parte sempre
dall'albero.

L'albero è letto pigramente: il contenuto di una directory viene
recuperato solo quando è mostrata o espansa per la prima volta, così
esplorare un volume enorme costa solo le directory realmente aperte. Una
directory che il chiamante non può elencare è rifiutata sul posto:
l'errore compare sulla riga dei messaggi e la vista precedente resta
com'era; nulla viene inventato.

Tasti:

- `Up`/`Down` o `k`/`j` — muovere l'evidenziazione della finestra
  attiva; `PageUp`/`PageDown` la spostano di una schermata, `Home`/`End`
  saltano alla prima o all'ultima riga. Muovendo l'evidenziazione
  dell'albero, la directory appena evidenziata viene elencata nella
  finestra dei file.
- `Right`/`l` — nell'albero, espandere il ramo evidenziato (letto
  pigramente); nella finestra dei file, scendere nella directory
  evidenziata.
- `Left`/`h` — nell'albero, comprimere il ramo evidenziato o — quando è
  già compresso o senza sottodirectory — risalire l'evidenziazione alla
  directory madre; nella finestra dei file, tornare all'albero.
- `+`/`-` — espandere / comprimere il ramo evidenziato dell'albero.
- `Enter` — nell'albero, passare alla finestra dei file; nella finestra
  dei file, scendere nella directory evidenziata (entrambe le finestre
  seguono) oppure aprire il file evidenziato in un visualizzatore a
  schermo intero: il visualizzatore del disassemblato per un eseguibile
  riconosciuto, il paginatore di testo quando i primi byte del file si
  leggono come testo, il dump esadecimale altrimenti (i visualizzatori
  sono descritti più avanti).
- `Esc` — annullare un calcolo dell'uso del disco in corso
  (conservandone le cifre); altrimenti tornare dalla finestra dei file
  alla finestra dell'albero.
- `o` — aprire il file evidenziato in un visualizzatore a scelta: `t`
  testo, `x` esadecimale o `d` disassemblato.
- `Tab` — passare tra la finestra dell'albero e la finestra dei file.
- `s` — aprire il menu di ordinamento: `n` nome, `e` estensione,
  `s` dimensione, `m` data di modifica, `r` inverte il verso, `Esc`
  annulla. Le directory sono sempre raggruppate prima dei file.
- `c` — copiare la voce selezionata: una riga chiede la destinazione.
  Una destinazione relativa finisce nella directory elencata; una
  destinazione che è una directory esistente riceve la copia al suo
  interno, con il nome dell'origine. `Tab` completa il percorso
  digitato — una corrispondenza unica viene inserita (una directory
  resta aperta con la sua `/`), più corrispondenze vengono estese fino
  alla radice comune o elencate. Una directory è copiata con tutto il suo
  contenuto. Copiare una voce su sé stessa o una directory nel proprio
  sottoalbero è rifiutato prima di scrivere qualsiasi cosa.
- `m` — spostare la voce selezionata, con la stessa richiesta di
  destinazione. Entro lo stesso volume lo spostamento è una rinomina
  atomica; tra volumi la voce viene copiata e poi l'origine rimossa.
- `r` — rinominare la voce selezionata sul posto: la riga è
  precompilata con il nome attuale.
- `d` — eliminare la voce selezionata dopo una conferma; solo `y`
  procede. Eliminare una directory rimuove tutto il suo contenuto, e la
  conferma lo dice. La conferma si può disattivare nel menu delle
  impostazioni (`S`).
- `M` — creare una directory nella directory elencata; il nome viene
  chiesto.
- `a` — aprire gli attributi della voce selezionata: i suoi bit di
  permesso e i suoi attributi estesi (coppie `namespace.key = value`, ad
  esempio `user.comment`). Nella vista, le frecce scorrono gli attributi,
  `m` modifica il modo (una riga ottale precompilata con i bit attuali;
  Invio applica — solo il proprietario della voce può cambiarlo, il
  kernel rifiuta chiunque altro), `n` aggiunge un attributo come riga
  `key=value`, Invio modifica quello selezionato (precompilato; con un
  valore binario si precompila solo la chiave), `d` lo rimuove ed Esc
  esce. Ogni modifica la decide il kernel — lettura e scrittura seguono i
  permessi propri della voce, gli spazi dei nomi `system.`/`trusted.`
  sono riservati e un rifiuto viene mostrato tale e quale. Un file
  system il cui formato non memorizza attributi (FAT32, ext4) lo segnala
  nella vista; l'editor del modo vi funziona comunque.
- `t` — marcare o smarcare la voce selezionata del pannello dei file e
  scendere di una riga; pressioni ripetute marcano quindi una serie.
  Le voci marcate portano un `*`.
- `T` — marcare per modello: un glob (`*`, `?`, `[...]`) confrontato
  con i nomi visibili, oppure un intervallo — `size:MIN..MAX` (byte,
  suffissi `K`/`M`/`G`/`T`, ciascun estremo può restare aperto:
  `size:1M..`, `size:..64K`) o `date:YYYY-MM-DD..YYYY-MM-DD` (data di
  modifica, data finale inclusa; una voce il cui supporto non conserva
  date non corrisponde mai a un intervallo di date). Ogni corrispondenza
  si aggiunge all'insieme marcato; un modello o un intervallo malformato
  non marca nulla e ne spiega il motivo.
- `i` — invertire le marcature sulle voci visibili.
- `C` — cancellare tutte le marcature.
- `f` — filtrare il pannello dei file con un glob sul nome dei file,
  applicato in tempo reale mentre lo si digita. `Enter` mantiene il
  filtro (mostrato nella riga di stato), `Esc` ripristina il filtro
  com'era. Un modello che non si compila non nasconde nulla — la riga di
  stato lo segnala invece con `(bad pattern)`. Svuotare il modello toglie
  il filtro.
- `/` — cercare nel ramo sotto la directory attiva con un glob sul nome
  dei file, confrontato con il percorso di ogni file relativo al ramo. I
  risultati arrivano nella vista appiattita man mano che vengono trovati.
- `F` — cercare un testo letterale nel contenuto dei file, senza
  distinguere maiuscole e minuscole e leggendo a finestre limitate (una
  corrispondenza a cavallo di un limite di lettura viene trovata
  comunque). Con voci marcate, la ricerca copre l'insieme marcato (le
  directory marcate ricorsivamente); altrimenti copre il ramo attivo.
  Ogni riga di risultato riporta il numero di corrispondenze; un file il
  cui contenuto sembra binario viene segnalato come corrispondenza
  binaria — i suoi byte non vengono mai mostrati. Un file che non si
  lascia leggere compare nel rapporto della scansione, mai scartato in
  silenzio.
- `u` — contare l'uso del disco sotto la directory attiva: file, byte
  e directory, percorsi in modo incrementale in secondo piano. `Esc`
  annulla conservando le cifre contate fin lì.
- `v` — appiattire il ramo sotto la directory attiva: un elenco di
  ogni file sottostante, riempito pagina per pagina (`Space` carica
  la successiva). Nella vista, `t`/`T`/`i`/`C` marcano le sue righe,
  `c`/`m`/`d` eseguono operazioni in blocco sull'insieme marcato,
  `Enter` salta alla directory della riga selezionata nei pannelli (con
  il cursore sulla sua voce) e `Esc` torna ai pannelli. Finché una
  scansione o una ricerca è ancora in corso, `Esc` prima la ferma,
  conservando le righe trovate fin lì. Le righe sono nominate
  relativamente al ramo appiattito. I risultati di ricerca (`/`, `F`)
  riempiono questa stessa vista, quindi le loro righe si possono marcare
  e trattare esattamente come un elenco appiattito.
- `H` — mostrare/nascondere le voci nascoste (nomi con punto) in entrambi
  i pannelli.
- `.` — ripetere l'ultima operazione sui file sulla selezione corrente:
  una copia o uno spostamento va di nuovo nella stessa directory di
  destinazione, un'eliminazione chiede di nuovo secondo l'impostazione
  di conferma. Se non c'è ancora alcuna operazione, il tasto lo dice.
- `V` — elencare i volumi montati: ogni riga mostra il punto di
  montaggio, il tipo di file system e i byte liberi/totali quando il
  volume li riporta. `Enter` sposta la radice dell'intera sessione sul
  volume scelto; `Esc` chiude l'elenco. Quando non viene riportato
  alcun volume, il tasto lo dice.
- `S` — il menu delle impostazioni: `1` attiva o disattiva la conferma
  dell'eliminazione singola, `2` quella dell'eliminazione in blocco. Le
  modifiche vengono salvate nelle impostazioni proprie di questa
  applicazione, che solo essa può leggere, e caricate all'avvio
  successivo; quando il servizio impostazioni non è raggiungibile,
  valgono solo per la sessione e il menu ne spiega il motivo.
- `?` — mostrare questo aiuto sopra i pannelli; qualsiasi tasto lo chiude.
- `q` — uscire ripristinando il terminale.

Finché ci sono voci marcate, `c`, `m` e `d` agiscono sull'intero
insieme marcato invece che sulla selezione: `c`/`m` chiedono una
directory di destinazione esistente in cui le voci atterrano (`Tab` la
completa), e `d` conferma l'eliminazione in blocco (a meno che quella
conferma non sia disattivata nel menu delle impostazioni). Le voci sono
elaborate in ordine di marcatura; un errore non ferma mai il resto, il
rapporto finale conta ciò che è riuscito e una schermata di rapporto
nomina ogni errore — un blocco non è mai silenziosamente parziale. Le
voci riuscite vengono smarcate; gli errori restano marcati per
riprovare.

Quando una copia o uno spostamento sovrascriverebbe un file esistente,
la sessione chiede file per file: `o` sovrascrive, `s` salta
(un'origine saltata resta al suo posto) e `c` annulla i passi rimanenti —
in un blocco, annullare abbandona tutte le voci rimanenti —
ciò che è già stato applicato rimane, e il rapporto finale dice cosa è
successo. Un errore a metà copia rimuove la destinazione scritta a
metà e mostra l'errore del kernel; nulla si spaccia mai per una copia
completa. Ogni operazione è autorizzata dal kernel — un rifiuto
compare tale e quale sulla riga dei messaggi senza che nulla cambi.
Una copia conserva i buchi di un file e i suoi attributi estesi; ogni
attributo che la destinazione non può conservare viene indicato nella
riga dei messaggi al termine dell'operazione.

Una directory copiata o spostata su una directory con lo stesso nome viene
unita a essa: ciò che contiene confluisce nella directory già presente, e per
ciascuno dei suoi file già presenti viene chiesto allo stesso modo.

L'intestazione con le statistiche del disco in alto mostra il percorso
elencato, i byte liberi/totali del volume sottostante (quando il servizio
di informazioni di sistema può riferirli), il numero di elementi visibili
e — finché qualcosa è marcato — il numero di voci marcate. La banda di
stato sotto le finestre mostra la finestra attiva (Tree o Files),
l'ordinamento, se le voci nascoste sono visibili, il filtro attivo sul
nome dei file, e il numero di voci marcate con il loro totale di byte. Un
file il cui formato di archiviazione non conserva la data di modifica
mostra `-` nella colonna della data.

Quando il pannello dei file omette le voci nascoste, la sessione annota
anche l'omissione sul flusso di informazioni standard (fd 3) — un record
informativo per ogni cambiamento, catturabile con `fstree 3>info.jsonl`;
la sessione in sé non ne risente.

I visualizzatori: `Enter` su un file regolare lo apre in sola lettura in
un visualizzatore a schermo intero. Un eseguibile riconosciuto — un'immagine
`rxe`, un ELF a 64 bit, un modulo wasm o un manifesto firmato a sé stante
— si apre nel **visualizzatore del disassemblato**; un file i cui primi
byte sono privi di NUL e UTF-8 valido si apre nel **paginatore di
testo**; tutto il resto si apre nel **dump esadecimale**. `o` scavalca
questa scelta. `x` passa il visualizzatore aperto al dump esadecimale
nello stesso punto, `t` al paginatore di testo (allineato all'inizio
della riga che contiene l'offset mostrato) e `d` al visualizzatore del
disassemblato (un file che nessun formato contenitore rivendica chiede un
set di istruzioni e viene decodificato come frammento grezzo dal punto
attuale); `q` o `Esc` torna ai pannelli.

I visualizzatori scorrono il file a finestre limitate — il file non è mai
tenuto in memoria per intero, così un file di qualsiasi dimensione (ben
oltre 4 GiB) si sfoglia correttamente — e condividono i tasti:

- `Up`/`Down` o `k`/`j` — una riga; `PageUp`/`PageDown`, `b`/`Space` —
  una pagina; `Home`/`End` — l'inizio / l'ultima pagina.
- `g` — andare a un punto: un numero di riga a partire da 1 nel
  paginatore di testo, un offset in byte (decimale o esadecimale con
  `0x`) nel dump esadecimale, un indirizzo nel visualizzatore del
  disassemblato.
- `/` — cercare in avanti dal punto attuale; `n` ripete l'ultima ricerca
  oltre la corrispondenza precedente. La scansione gira in secondo piano
  a passi limitati — la sessione resta reattiva anche su un file enorme,
  la riga di stato dice `searching…` ed `Esc` la ferma sul posto.
- `?` — questo aiuto; qualsiasi tasto lo chiude.

Il **paginatore di testo** decodifica l'UTF-8 con una sostituzione
visibile per i byte non validi, espande le tabulazioni e mostra ogni
altro byte di controllo come un `·` visibile — il contenuto dei file non
viene mai inviato al terminale come sequenze di escape grezze. Le righe
lunghe vanno a capo per impostazione predefinita; `w` attiva o disattiva
l'a capo (senza, la coda viene tagliata al bordo destro). `/` cerca testo
letterale, senza distinguere maiuscole e minuscole, oltre i limiti di
lettura. La riga di stato mostra il punto come numero di riga; dopo un
salto dal dump esadecimale il numero di riga è sconosciuto (il conteggio
è stato risparmiato) finché un salto con `g` non lo riancora.

Il **dump esadecimale** mostra la disposizione classica — colonna degli
offset, sedici byte come coppie esadecimali e una colonna ASCII con i
byte non stampabili resi come `.`. La colonna degli offset è larga
quanto serve al file (almeno otto cifre esadecimali). `/` cerca testo
letterale (senza distinguere maiuscole e minuscole) oppure, scritto come
`0x` seguito da coppie di byte esadecimali (`0xdeadbeef`), una sequenza
esatta di byte.

Il **visualizzatore del disassemblato** si apre su una pagina di
riepilogo: il formato del contenitore, il set di istruzioni e il punto di
ingresso, le sue regioni (indirizzo, estensione nel file, dimensione in
memoria, permessi) e il numero di simboli; un manifesto firmato a sé
stante elenca invece la propria versione ABI e le capacità che richiede.
Un'immagine `rxe` porta il proprio manifesto accanto a sé, mai
incorporato, e il riepilogo lo dice. `Up`/`Down` scorrono le righe delle
regioni e `Enter` apre il disassemblato della regione di codice
selezionata (una regione di dati appare in esadecimale sui suoi byte del
file). Un contenitore che non indica alcun set di istruzioni — un'immagine
`rxe` gira su qualunque macchina la carichi — lo chiede una volta:
`x` x86-64, `a` aarch64, `r` riscv64, `w` wasm.

Il pannello del codice mostra un'istruzione per riga — indirizzo, byte
di codifica, mnemonico, operandi — con righe di etichetta `<symbol>:` e
destinazioni di salto simbolizzate (`<main+0x8>`) dove il contenitore
indica i simboli. Le istruzioni vengono decodificate una schermata alla
volta, mai l'intero binario in anticipo. `g` salta a un indirizzo, `/`
cerca nel testo delle istruzioni, `End` avanza fino alla fine della
regione in secondo piano (`Esc` ferma l'avanzamento) e `I` ridecodifica
con un altro set di istruzioni. `Esc` torna alla pagina di riepilogo.

Ogni decodifica di contenitori e istruzioni gira in un processo ausiliario
blindato (la sandbox dei parser), mai nel gestore di file stesso: un
eseguibile malevolo può far cadere l'ausiliario, non la sessione. Un file
che il decodificatore rifiuta — malformato, o troppo grande da passare
alla sandbox — ripiega sul dump esadecimale con un avviso di una riga.

Una lettura che il kernel rifiuta durante la visualizzazione chiude il
visualizzatore e mostra l'errore sulla riga dei messaggi — un contenuto
non più attuale non viene mai mostrato come aggiornato.

## OPTIONS

- `directory` — la directory in cui inizia la sessione; il valore
  predefinito è la vista radice `/`.
- `-h`, `-?` — stampare la forma breve di questo documento e uscire.

## EXIT STATUS

- `0` — la sessione è terminata con la `q` dell'utente.
- `1` — la directory iniziale non è stata elencabile, o il percorso del
  terminale è fallito. Anche una riga di comando non compresa termina
  con `1`.

## SEE ALSO

ls, cp, mv, rm, mkdir, chmod, du, df, find
