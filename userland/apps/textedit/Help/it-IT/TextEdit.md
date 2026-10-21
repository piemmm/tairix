## NAME

TextEdit — editor grafico di testo ed esadecimale

## SYNOPSIS

`TextEdit`

## DESCRIPTION

Modifica qualsiasi file in una finestra del desktop: testo, codice sorgente,
i file di impostazioni del sistema o byte grezzi. Avviato con un documento —
dal gestore dei file, dal desktop o trascinando un file sulla sua icona nella
barra delle icone — apre una finestra su quel file. Avviato da solo apre una
finestra vuota. Ogni documento è una finestra dell'unico editor; chiudere
l'ultima lo lascia nella barra delle icone, e la riga «Esci» del menu della
sua icona lo termina.

Nulla di ciò che un file contiene resta nascosto. Un byte di controllo appare
come `[x03]`, un byte che non è UTF-8 valido come `[xC3]`, e un carattere
invisibile o che cambia la direzione di scrittura come `[U+202E]`, ciascuno
nel proprio colore e ciascuno un solo passo del cursore. Un file che sembra
contenere dati binari si apre nella vista esadecimale, che mostra ogni byte
come due cifre esadecimali accanto al suo carattere e modifica gli stessi
byte della vista testo.

Un eseguibile — un'immagine di programma TAIRiX, un driver, un file ELF64 o
un modulo WebAssembly — si apre invece nella vista codice: ciò che dicono le
sue intestazioni, ogni parte alla sua posizione con la sua lunghezza e ciò
che dichiara — la versione del sistema per cui è stato compilato, il suo
punto d'ingresso, i suoi segmenti e le librerie di cui ha bisogno, il
manifesto firmato di un driver con le capacità che richiede e l'hardware a
cui si associa, le intestazioni di programma e di sezione di un file ELF, le
sezioni di un modulo — poi le istruzioni di ogni regione di codice, una per
riga con il suo indirizzo e i suoi byte di codifica, etichettate con i
simboli da cui iniziano e con i simboli raggiunti dai loro salti. Chiavi e
firme sono mostrate così come sono scritte, senza essere verificate. La vista
codice è di sola lettura; le viste testo ed esadecimale modificano lo stesso
file. Il codice viene decodificato da un secondo processo di lavoro tutto
suo, anch'esso senza alcun accesso, e letto nel set di istruzioni indicato
dal file, altrimenti in quello di questa macchina; «Set di istruzioni» nel
menu Vista lo legge in un altro, e il campo della vista nella riga di stato
offre la vista codice accanto a testo ed esadecimale.

Un'immagine — PNG, JPEG, GIF, BMP, TIFF, WEBP, un file di icona o di
puntatore, un'area di sprite di RISC OS o un documento OpenRaster — si apre
invece nella vista dei dati: di che cosa è fatto il file, ogni record alla
sua posizione con la sua lunghezza e ciò che dichiara, ogni tag EXIF per
nome con il suo valore, poi ogni byte del file in esadecimale accanto al suo
carattere. Anche un manifesto firmato — l'`AppInfo` di un'applicazione o il
manifesto di un programma — si apre lì: ciò che nomina, le capacità che
richiede, i tipi di file che rivendica e i servizi che cerca, poi i suoi
byte. La vista dei dati è di sola lettura; la vista esadecimale modifica gli
stessi byte. Un'immagine viene letta da un terzo processo di lavoro tutto
suo, un manifesto da quello della vista codice, nessuno dei due con alcun
accesso, e il menu Vista offre la vista dei dati accanto a testo ed
esadecimale.

Il codice sorgente viene colorato: HTML, XML e SVG, CSS, JavaScript, JSON,
YAML, TOML, Markdown, Rust, C, Java, Python e script di shell. Anche i file di
impostazioni del sistema — impostazioni delle applicazioni, la libreria dei
programmi, la configurazione del sistema e della rete, le sostituzioni dei
servizi, le basi dati di utenti e gruppi e i manifesti delle famiglie di
caratteri — vengono colorati e verificati con l'analizzatore con cui il
sistema li legge: un problema è segnato nel margine accanto alla sua riga e
riportato nella riga di stato. Il formato è scelto dal nome del file, poi dai
suoi primi byte; un formato scelto dal menu Vista o dalla riga di stato
prevale sempre.

L'editor non possiede alcuna capacità sul file system. Modifica soltanto il
file che gli è stato consegnato. Un file che l'utente può modificare viene
consegnato in scrittura, e Salva lo riscrive; ogni altro è di sola lettura, e
Salva chiede dove salvarne una copia. Colorazione, rilevamento del formato e
verifica girano in un processo di lavoro separato privo di qualsiasi accesso,
così un file ostile non può raggiungere nulla di ciò che raggiunge l'editor.

Premendo il tasto secondario (destro) del mouse in qualsiasi punto della
finestra si apre il suo menu: Taglia, Copia, Incolla e Seleziona tutto, poi
File, Modifica, Trova e Vista, ciascuno dei quali apre il proprio
sottomenu. La finestra non ha una barra dei menu.

La riga di stato mostra la riga e la colonna del cursore, ciò che la verifica
ha trovato e, come campi che aprono un menu al clic: il formato, testo o
esadecimale, i fine riga e il rientro. Chiudere una finestra o uscire con
modifiche non salvate chiede prima conferma. Chiudere una finestra con il
tasto secondario (destro) sul suo pulsante di chiusura mostra anche il suo
file nel gestore dei file, nella cartella che lo contiene.

* `Ctrl+N` — una nuova finestra
* `Ctrl+O` — aprire un file
* `Ctrl+S` — salvare; `Ctrl+Shift+S` — salvare con nome
* `Ctrl+W` — chiudere la finestra
* `Ctrl+Z` — annullare; `Ctrl+Shift+Z` o `Ctrl+Y` — ripetere
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — tagliare, copiare, incollare
* `Ctrl+A` — selezionare tutto
* `Ctrl+F` — cercare; `Ctrl+H` — sostituire
* `F3` / `Shift+F3` — la corrispondenza successiva o precedente
* `Ctrl+L` — andare a una riga
* `F8` — il problema successivo trovato dalla verifica
* `Ctrl+]` / `Ctrl+[` — aumentare o ridurre il rientro delle righe scelte
* `Ctrl+/` — trasformare in commento le righe scelte o ripristinarle
* `Ctrl+Shift+H` — passare dalla vista testo a quella esadecimale e ritorno
* `Insert` — passare dall'inserimento alla sovrascrittura e ritorno
* `Tab` — nella vista esadecimale, spostarsi tra le colonne esadecimale e dei caratteri

## OPTIONS

`-h`, `-?`, `--help`
: Scrivere questa guida sull'output standard e terminare.

## EXIT STATUS

Zero dopo Esci. Diverso da zero quando il canale della finestra, la casella
degli eventi o la sessione del desktop è stata rifiutata; il motivo è
dichiarato sul flusso di errore standard.
