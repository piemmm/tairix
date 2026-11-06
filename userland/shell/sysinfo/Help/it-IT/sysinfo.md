## NAME

sysinfo — interrogare le informazioni di sistema

## SYNOPSIS

`sysinfo <query>`

## DESCRIPTION

Invia un'interrogazione tipizzata all'API di informazioni di sistema e
ne mostra la risposta. TAIRiX non ha `/proc` né `/sys`: questo comando
è il volto da terminale della stessa API versionata e controllata da
capacità che ogni programma usa, e nessun percorso elude il controllo
di capacità.

Le interrogazioni:

- `processes`, `ps` — elencare i processi, una riga per processo.
- `memory`, `mem` — statistiche di memoria del kernel (richiede
  `CAP_SYSINFO_KERNEL`).
- `hardware`, `hw` — l'albero hardware rilevato (richiede
  `CAP_SYSINFO_HW`).
- `identity`, `id` — identità della macchina e versione del SO.
- `uptime` — tempo dall'avvio e l'ora di avvio.
- `limits`, `rlimits` — i propri limiti di risorse effettivi e il loro
  uso in tempo reale.
- `seats` — l'inventario dei posti: il proprietario di ogni display e
  la sua console in primo piano (richiede `CAP_SYSINFO_HW`).
- `pressure` — l'indicatore di pressione della memoria in tempo reale:
  banda, soglie e contatori di transizione (richiede
  `CAP_SYSINFO_KERNEL`).
- `reclaim` — il registro delle cache recuperabili, una riga per classe
  (richiede `CAP_SYSINFO_KERNEL`).
- `ramzip` — i contatori del livello di memoria compressa (richiede
  `CAP_SYSINFO_KERNEL`).
- `cpu` — profondità della coda, cambi di contesto e prelazioni per CPU
  (richiede `CAP_SYSINFO_KERNEL`).
- `cpuinfo` — il rapporto sul processore per CPU (un soprainsieme di
  `/proc/cpuinfo`): modello/produttore, classe di prestazioni, flag delle
  estensioni ISA, il registro di identità grezzo, la frequenza di clock
  del core misurata dal vivo (in MHz — o un onesto «unknown» dove non
  esiste alcun contatore di clock del core) e la frequenza fissa di
  riferimento o base tempi. Dati pubblici sull'hardware, nessuna
  capacità richiesta.
- `irq`, `irqs` — la tabella IRQ del kernel: una riga per ogni linea di
  interrupt associata — il suo id, il task del driver proprietario, il
  numero di interrupt dall'avvio e se la linea è in quarantena (richiede
  `CAP_SYSINFO_HW`).
- `frames` — quanto sono costati i fotogrammi composti di ogni sessione
  desktop: una riga per ogni sessione che pubblica — i pixel che i suoi
  fotogrammi hanno ricomposto e la media per fotogramma, il multiplo di
  sovradisegno (contributi di livello per pixel danneggiato), la quota
  risolta copiando un tratto opaco, i pixel che uno sfondo in vetro
  smerigliato ricalcolato ha sfocato di nuovo, i pixel convertiti per lo
  scan-out, il fotogramma singolo peggiore e la sua quota dello schermo,
  le chiamate al driver che li hanno presentati, e gli hit e i miss della
  cache delle decorazioni delle finestre. Sono le cifre del desktop stesso
  — solo un processo che detiene un compositore può contare i pixel —
  quindi ogni riga nomina chi l'ha pubblicata, come attestato dal
  servizio, e una sessione che non ha pubblicato nulla non produce alcuna
  riga (richiede `CAP_SYSINFO_GLOBAL`).
- `storage`, `io` — tre tabelle per volume, in ciascuna una riga per ogni
  volume a blocchi consapevole dei guasti, contrassegnata da un prefisso
  dell'identificatore durevole del volume e con il nome del dispositivo
  che lo serve. Quell'identità è l'endpoint del servizio a blocchi che lo
  serve quando un driver in spazio utente serve il dispositivo, e
  un'identità riservata gestita dal kernel quando è il kernel stesso a
  gestirlo — il disco da cui la macchina si è avviata, che non ha alcun
  endpoint di servizio. **Servizio** nomina inoltre il dispositivo: il
  nome breve dichiarato dal suo driver (`virtio-blk`, `emmc2`,
  `usb-msd`), oppure `-` se non ne dichiara alcuno. Riporta i byte
  cumulativi, le richieste completate e il tempo di occupazione del
  dispositivo, da cui un lettore ricava per differenza throughput, IOPS,
  utilizzo e attesa — nulla è precalcolato, quindi due lettori non
  ereditano mai la stessa finestra di media (nessuna capacità
  richiesta). **Coda** riporta ciò che è in sospeso adesso, gli
  accumulatori da cui per differenza si ricava una profondità media, e la
  profondità e la scadenza consentite dalla classe del dispositivo.
  **Salute** riporta la disponibilità corrente del volume
  (available/degraded/recovering/lost) e i contatori cumulativi degli
  esiti (completamenti, reset, timeout, errori del supporto, riemissioni)
  su cui un disco guasto o instabile diventa visibile. Le ultime due
  richiedono `CAP_SYSINFO_KERNEL`; la tabella del servizio viene stampata
  per prima, così un chiamante che non la possiede legge comunque le
  cifre di utilizzo prima che venga segnalato il rifiuto.
- `raid`, `arrays` — gli array RAID composti e i dispositivi che il
  compositore di array detiene: una riga per array — un prefisso della
  sua identità, il suo livello, la sua salute
  (optimal/degraded/recovering/failed), il numero di membri sincronizzati
  e definiti, la sua unità di striping, il suo numero di blocchi e
  qualsiasi ricostruzione o verifica in corso — poi una riga per
  dispositivo — il suo nodo dell'albero hardware, l'array a cui
  appartiene (un trattino per un candidato non affiliato), il suo slot,
  il suo ruolo (candidate/held/in-sync/resyncing/faulted), la sua
  dimensione e la generazione di metadati che porta (richiede
  `CAP_SYSINFO_HW`).
- `dma`, `iommu` — le unità di traduzione DMA: una riga per unità — il suo
  nodo nell'albero hardware, la sua famiglia, se traduce o perché no, come
  segnala i suoi errori, dove risiedono le sue traduzioni, i proprietari e i
  flussi del firmware che detiene, e i suoi errori registrati e scartati, e
  i flussi che una tempesta ha silenziato, dall'avvio — poi una riga per
  gruppo di isolamento detenuto da un proprietario — la sua unità, il nodo e
  la generazione del detentore e lo stato di quel proprietario — poi una
  riga per nodo che un'unità traduce per un proprietario — la sua unità e il
  suo gruppo, la generazione e lo stato del suo proprietario, i suoi flussi,
  e le mappature del suo dominio con i byte che coprono (richiede
  `CAP_SYSINFO_HW`).
- `audio`, `sound` — i dispositivi audio: una riga per uscita e per
  ingresso — il suo identificativo, se è il predefinito della sua
  direzione, il livello e l'esclusione, la frequenza misurata del suo
  orologio durante la riproduzione, i fotogrammi persi, la posizione e il
  nome — poi una riga per ogni flusso audio tuo — il dispositivo, la
  direzione, il ruolo e lo stato, la posizione, i difetti e chi lo detiene.
- `show <resource-ref>` — legge un riferimento a risorsa
  `info:`/`state:`/`stats:` e stampa il suo valore. Quegli spazi dei nomi
  servono valori tipizzati tramite questa API, mai flussi di byte: `cat` non
  può aprirli. Un rifiuto nomina la capability necessaria.
- `describe <resource-ref>` — stampa la busta della risposta invece del
  valore: il produttore, l'autorizzazione con cui è stata servita e i
  metadati del payload — per una metrica il genere, l'unità, il
  comportamento di azzeramento e la finestra di campionamento; per un fatto
  il tipo e la riservatezza.
- `help` — la guida breve di questo comando.

Senza interrogazione viene mostrata la guida breve.

## OPTIONS

- `--all, -a` — con `processes`: elencare tutti i processi del sistema
  anziché solo i propri; il servizio concede questa vista solo a un
  chiamante che detiene `CAP_SYSINFO_GLOBAL`.
  Con `audio`, tutti i flussi audio della macchina, con la stessa
  capacità.
- `-h, -?` — mostrare la guida breve di questo comando.

## EXAMPLES

- `sysinfo identity` — stampare l'identità della macchina e la versione
  del SO.
- `sysinfo ps --all` — elencare tutti i processi del sistema.
- `sysinfo frames` — mostrare quanto costano i fotogrammi del desktop.

## EXIT STATUS

- `0` — l'interrogazione è stata risposta e mostrata.
- `1` — il servizio ha rifiutato o è fallito, oppure il risultato non è
  stato consegnato. Anche una riga di comando non compresa termina con
  `1`.

## ENVIRONMENT

- `LANG` — la locale preferita per la guida breve (un tag BCP-47 come
  `it-IT`).

## SEE ALSO

- `man`
- `ps`
- `top`
