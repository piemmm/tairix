## NAME

configure — leggere e modificare le impostazioni della macchina

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

Elenca, mostra e modifica le impostazioni della macchina, conservate in
`/System/Settings/Configuration/system.conf`. Senza operandi ogni voce è
elencata con il valore attuale; con una sola chiave ne è mostrato il
valore; con una o più coppie `<key> <value>` quelle voci vengono
modificate insieme.

Più coppie formano un'unica modifica, non diverse: ogni coppia viene
verificata prima che qualcosa cambi, e le voci vengono poi salvate in
una volta sola. Una riga di comando che nomina una chiave sconosciuta,
un valore fuori dall'insieme della sua chiave o due volte la stessa
chiave non cambia assolutamente nulla, così un gruppo di voci non può
mai restare applicato a metà.

Per modificare una voce serve un amministratore: un account ordinario
legge le impostazioni della macchina ma non ne modifica nessuna. Il
sistema fornisce impostazioni proprie, che valgono finché un
amministratore non ne modifica una, e la prima modifica conserva tutte
le altre impostazioni fornite dal sistema. Una modifica ha effetto la
prossima volta che si avvia la parte del sistema che la legge
(`os.loginType` al prossimo accesso, gli interruttori `cache.*` al
prossimo avvio), salvo che la sua chiave qui sotto indichi altrimenti.

L'insieme delle chiavi è chiuso: una chiave sconosciuta, o un valore
fuori dall'insieme di una chiave, è rifiutato indicando le scelte
valide e non cambia nulla.

- `os.loginType` — `text` o `graphical`: quale tipo di sessione il
  servizio di accesso avvia per un utente autenticato. `graphical` (il
  predefinito) avvia direttamente la sessione desktop dopo
  l'autenticazione, ripiegando sull'accesso testuale su una macchina che
  non può eseguirne una; `text` avvia la shell dell'account — il desktop
  si avvia solo da un accesso grafico.
- `cache.all` — `on` o `off`: l'interruttore principale della cache.
  `on` (il predefinito) lascia che ogni classe di cache sottostante
  segua la propria impostazione; `off` è un tetto che disabilita ogni
  cache in memoria a prescindere dalle impostazioni per classe.
- `cache.filesystem`, `cache.block`, `cache.transform`,
  `cache.semantic` — `auto` o `off`: gli interruttori per classe per le
  quattro cache di memoria recuperabili (le cache del file system, del
  blocco dell'intero disco, del cluster decompresso e dell'avvio delle
  applicazioni). `auto` (il predefinito) lascia che il gestore della
  pressione di memoria governi la classe; `off` la disabilita del
  tutto. Non esiste un `on` per classe: una classe non può essere
  forzata a ignorare la pressione di memoria. Una classe è di fatto
  `off` ogni volta che `cache.all` è `off`.

Ogni cache è un acceleratore recuperabile, mai la fonte di verità,
quindi spegnerne una o tutte rende soltanto più lento il lavoro
interessato — non cambia mai un risultato.

- `net.ipv4.enabled`, `net.ipv6.enabled` — `true` o `false`: gli
  interruttori delle famiglie di indirizzi a livello di stack. Entrambi
  sono `true` per impostazione predefinita. Una famiglia disattivata non
  assegna indirizzi, non risponde ad alcun pacchetto e rifiuta un socket
  di quella famiglia con un errore tipizzato — mai uno scarto
  silenzioso.
- `net.ipv6.privacy` — `true` o `false`: se lo stack forma indirizzi
  IPv6 temporanei (di privacy) oltre a quello stabile. `false` (il
  valore predefinito) usa solo l'indirizzo SLAAC stabile.
- `net.tcp.syncookies` — `auto` o `always`: la difesa dalle inondazioni
  SYN. `auto` (il valore predefinito) mantiene una coda semiaperta
  limitata e ripiega su cookie senza stato in caso di overflow;
  `always` risponde a ogni richiesta di connessione senza stato. Non
  esiste `off` — una coda di connessioni indifesa non è
  un'impostazione.
- `net.tcp.keepalive` — `true` o `false`: se le connessioni TCP inviano
  sonde keepalive su un collegamento inattivo. `false` (il valore
  predefinito) non sonda mai e non chiude mai una connessione inattiva;
  `true` sonda un pari inattivo dopo l'intervallo consueto e chiude la
  connessione se smette di rispondere.
- `net.tcp.ecn` — `true` o `false`: se le connessioni TCP negoziano la
  notifica esplicita di congestione (ECN). `false` (il valore
  predefinito) lascia le connessioni Not-ECT; `true` offre ECN
  nell'handshake e poi tratta un contrassegno di congestione come un
  segnale di rallentamento invece di forzare la perdita di un pacchetto.
- `net.sockets.mem` — `auto` oppure una dimensione in byte come `64M`:
  la memoria che lo stack di rete può tenere in stato dei socket fra tutti
  i principali. `auto` (il valore predefinito) la dimensiona sulla RAM
  della macchina, così un server grande non resta legato a una cifra
  scelta su uno piccolo; una dimensione la sostituisce per un carico che
  conoscete meglio. Ogni principale può tenere un sedicesimo del
  bilancio effettivo, così c'è sempre posto per sedici. Byte anziché un
  numero di socket, perché lo stesso numero di socket sono pochi
  kilobyte da inattivi e megabyte a buffer pieni: il bilancio porta
  molte connessioni tranquille o meno connessioni attive, secondo quel
  che il carico è davvero.
- `time.servers` — `none` oppure un elenco di server di ora di rete
  separati da virgole, ciascuno un nome host o un indirizzo. `none` (il
  valore predefinito) non indica alcun server proprio: l'orologio viene
  allora impostato dal server indicato dalla concessione d'indirizzo della
  rete, altrimenti dal gruppo pubblico `pool.ntp.org`. I server indicati
  qui hanno la precedenza su entrambi.
- `time.refresh` — `6h`, `12h`, `1d`, `2d` o `7d`: quanto tempo di
  attività passa fra due interrogazioni dell'orologio una volta nota
  l'ora. `1d` è il valore predefinito. Un orologio non impostato, non
  plausibile o molto vecchio viene corretto appena la rete lo consente,
  qualunque cosa dica questa impostazione.
- `time.network` — `on` (il valore predefinito) o `off`: se l'orologio
  viene impostato dalla rete. Con `off` nessuna richiesta di ora parte
  verso alcun server, e l'orologio si imposta a mano o dal chip
  dell'orologio della scheda, letto comunque a ogni avvio. Una modifica si
  applica subito; l'impostazione risiede sul disco di sistema, quindi
  all'inizio di un avvio vale il valore predefinito finché quel disco non
  è raggiunto.
- `time.zone` — `default` oppure il nome di un fuso orario come
  `Europe/London`: il fuso orario della macchina, quello che mostra la
  schermata di accesso e che usa un account che non ne ha scelto alcuno.
  `default` è il fuso che il sistema fornisce come proprio; un nome che
  l'elenco dei fusi del sistema non contiene viene ignorato come se
  fosse `default`. Una modifica si applica subito.
- `input.mouse.debounce` — millisecondi interi, `25` per impostazione
  predefinita, `0` per disattivarlo, al massimo `100`: quanto tempo dopo il
  rilascio di un pulsante del mouse la pressione successiva dello stesso
  pulsante viene ignorata come rimbalzo del contatto invece di essere
  trattata come un nuovo clic. Un interruttore consumato può segnalare una
  seconda pressione pochi millisecondi dopo il rilascio pur intendendo un
  solo clic. Impostare `0` per un mouse il cui modo a raffica invia coppie di
  clic intenzionali.
- `audio.output`, `audio.input` — `auto` per impostazione predefinita, o la
  posizione di un terminale come la mostra `audioctl` (sedici cifre
  esadecimali, un punto e l'indice del terminale): l'uscita e l'ingresso
  che questa macchina preferisce come predefiniti. La scelta di una
  sessione viene prima; questa è la scelta prima di ogni accesso e su una
  macchina senza desktop. `auto` è il primo dispositivo trovato.
- `audio.level` — in decibel, `0dB` per impostazione predefinita, o
  un'attenuazione come `-12dB` o `-6.5dB`, con al massimo due decimali: il
  livello da cui parte ogni uscita e ingresso. Mai sopra `0dB`. Il servizio
  audio adotta un valore cambiato all'avvio successivo; `audioctl` cambia
  la macchina in funzione.
- `cooling.fan.mode` — `off` per impostazione predefinita, `on` o `auto`: se
  la ventola della CPU sul connettore GPIO della scheda non viene pilotata,
  gira sempre o viene comandata dalla temperatura del processore.
- `cooling.fan.line` — `none` per impostazione predefinita, o la linea GPIO
  a cui è collegata la ventola, da `0` a `63`. Si può pilotare solo una
  linea che il connettore della scheda mette a disposizione.
- `cooling.fan.signal` — `active-high` per impostazione predefinita, o
  `active-low`: quale livello della linea fa girare la ventola.
- `cooling.fan.start`, `cooling.fan.hysteresis` — gradi Celsius interi, `65`
  e `10` per impostazione predefinita: una ventola automatica parte a
  `start`, da `40` a `85`, e si ferma quando il processore si è raffreddato
  di `hysteresis`, da `2` a `20`. Una modifica a un'impostazione della
  ventola ha effetto subito.

Una chiave che non è nell'elenco qui sopra viene cercata in un secondo
registro: le impostazioni di rete di ciascuna interfaccia, conservate in
`/System/Settings/Network/network.conf`. Le sue chiavi si scrivono
`<interface>.<setting>`, dove `<interface>` è l'alias che un
amministratore ha dato a un'interfaccia di rete (`wan`, `lan0`) e
`<setting>` è uno tra `kind`, `match.mac`, `match.node`, `ipv4.method`,
`ipv4.address`, `ipv4.gateway`, `ipv6.method`, `ipv6.address`,
`ipv6.gateway`, `dns.servers`, `mtu`, `bond.members`, `bond.mode`,
`bond.monitor-interval` e `bond.primary`.

Il registro della macchina qui sopra viene sempre consultato per primo,
così un alias d'interfaccia non può mai appropriarsi del nome di
un'impostazione della macchina.

Le impostazioni di rete non hanno valori predefiniti, quindi l'elenco
mostra solo le impostazioni definite, e mostrarne una non definita
risponde con una riga vuota anziché inventare un valore. Solo un
amministratore può leggerle o modificarle: indicano l'hardware di ogni
interfaccia e gli indirizzi di questa macchina. Per chiunque altro
l'elenco mostra le impostazioni della macchina e avvisa che quelle di
rete non sono mostrate.

Poiché quel registro non ha valori predefiniti, un'impostazione viene
rimossa anziché ripristinata, e la si rimuove con un **valore vuoto**:
`configure wan.ipv4.address ""`. Nessuna impostazione d'interfaccia
accetta un valore vuoto, quindi questa scrittura non può mai essere
scambiata per l'impostazione di un valore. È ciò che permette a un solo
comando di passare un'interfaccia da un indirizzo statico a DHCP —
`configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
— una modifica di cui nessuna delle due metà, da sola, descrive una
configurazione coerente. Un'interfaccia di cui si rimuove l'ultima
impostazione non è più dichiarata affatto.

L'intera configurazione modificata viene verificata prima di salvare
alcunché: una modifica che la lascerebbe incoerente (un metodo statico
senza indirizzo, un'aggregazione di collegamenti con meno di due membri)
è rifiutata e non si salva nulla.

Modificare un'impostazione `net.*`, o una d'interfaccia, la salva e la
consegna allo stack di rete in esecuzione, così ha effetto subito.
Vengono consegnate solo le interfacce davvero toccate dalla modifica. Se
lo stack in esecuzione non accetta una consegna — non ce n'è nessuno in
esecuzione, o il vostro account non può amministrare la rete —
l'impostazione viene comunque salvata e `configure` lo segnala; si
applica allora al prossimo avvio.

Due limiti vengono segnalati anziché nascosti. Un'interfaccia che avete
rimosso dalla configurazione continua a funzionare fino al prossimo
avvio con la configurazione data per ultima allo stack, perché lo stack
non ha alcun messaggio che ne ritiri una. Un'interfaccia salvata senza
`match.mac` né `match.node` non può mai essere associata a un
dispositivo, quindi viene salvata e il rifiuto viene indicato.

## OPTIONS

- `-h, -?` — mostrare la guida breve di questo comando.

## EXAMPLES

- `configure` — elencare tutte le voci.
- `configure os.loginType` — mostrare il tipo di sessione predefinito.
- `configure os.loginType graphical` — avviare nell'accesso grafico.
- `configure cache.all off` — disabilitare ogni cache in memoria in
  tutto il sistema.
- `configure cache.filesystem off` — disabilitare solo la cache del
  file system.
- `configure net.ipv6.enabled false` — disattivare IPv6 in tutto lo
  stack.
- `configure time.servers 0.example.test,1.example.test` — impostare i
  server di ora di rete con cui si sincronizza l'orologio.
- `configure time.zone Europe/London` — tenere la macchina sull'ora di
  Londra.
- `configure wan.ipv4.address` — mostrare l'indirizzo IPv4 statico
  configurato per l'interfaccia chiamata `wan`.
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  dare a `wan` un indirizzo IPv4 statico e un gateway predefinito.
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — passare `wan` dall'indirizzamento statico a DHCP.
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — impostare i server
  DNS ricorsivi da usare su `wan`.

## EXIT STATUS

- `0` — elenco, valore, guida breve o modifica completati.
- `1` — le impostazioni non si sono potute leggere o modificare (per
  esempio il chiamante non può cambiare le impostazioni della macchina,
  o non può leggerne le impostazioni di rete), la chiave è sconosciuta,
  il valore è fuori dall'insieme della chiave, la modifica lascerebbe
  incoerente la configurazione di rete, oppure l'output non si è potuto
  consegnare. Anche una riga di comando non compresa termina con `1`.

## ENVIRONMENT

- `LANG` — la lingua preferita della guida breve (un'etichetta BCP-47
  come `fr-FR`).

## SEE ALSO

- `man`
