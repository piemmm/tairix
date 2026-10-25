## NAME

rm — dileu ffeiliau a chyfeiriaduron

## SYNOPSIS

`rm [-dfiIrRv] [--] file...`

## DESCRIPTION

Mae'n dileu pob operand ffeil, yn eu trefn. Datgysylltir operand nad
yw'n gyfeiriadur — dilëir cyswllt symbolaidd ei hun, byth yr hyn y mae'n
ei enwi; dim ond gydag `-r` y dilëir operand cyfeiriadur (sy'n dileu ei
gynnwys ddyfnder-yn-gyntaf ac yna'r cyfeiriadur ei hun) neu, pan fo'n
wag, gyda `-d`. Gyda'r naill neu'r llall, gwrthodir operand sydd â `.`
neu `..` yn gydran olaf iddo, fel y mae POSIX yn ei fynnu.

Gydag `-f`, hepgorir operand nad yw'n bodoli yn dawel ac ni ofynnir
byth gwestiwn. Mae `-i` yn gofyn ar ffrwd y gwall safonol cyn pob
dileu, gan enwi'r hyn y byddai'n ei ddileu
(`remove regular empty file 'a'?`, `remove symbolic link 'l'?`), a chyn
disgyn i gyfeiriadur sy'n cynnwys unrhyw beth; mae `-I` yn gofyn unwaith
ymlaen llaw cyn dileu mwy na thri operand neu cyn dileu ailadroddus. Mae
cwestiwn a wrthodwyd yn cadw'r gwrthrych (neu'n hepgor y rhediad cyfan,
yn achos `-I`) heb wall; ni thrinnir ateb annarllenadwy byth fel
cydsyniad. Y diweddaraf o `-f`, `-i` ac `-I` sy'n ennill.

Heb `-f`, pan fo'r mewnbwn safonol yn derfynell, gofynnir am ffeil na
chaiff y galwr ysgrifennu iddi cyn ei dileu
(`remove write-protected regular file 'x'?`), ac fe'i dilëir dim ond ar
ateb yn dechrau ag `y` neu `Y`; ni ofynnir byth am gyswllt symbolaidd. O
dan `-i` mae'r cwestiwn yn enwi'r amddiffyniad hefyd.

Adroddir am lwybr na ellir ei archwilio, ei ddarllen na'i ddileu, a
dilëir y gweddill o hyd; cedwir cyfeiriadur sy'n dal i gynnwys rhywbeth
heb ddweud gair. Gwrthodir dileu `/` yn ailadroddus, sut bynnag y'i
sillefir, o dan `--preserve-root`, y rhagosodiad. Mae `--` yn gorffen
dosrannu opsiynau: mae pob ymresymiad diweddarach yn llwybr.

## OPTIONS

- `-r, -R, --recursive` — dileu cyfeiriaduron a'u cynnwys.
- `-f, --force` — anwybyddu operandau nad ydynt yn bodoli; peidio byth
  â holi.
- `-d, --dir` — dileu cyfeiriaduron gwag.
- `-i, --interactive` — holi cyn pob dileu; dim ond ateb yn dechrau ag
  `y`/`Y` sy'n cydsynio.
- `-I` — holi unwaith cyn dileu mwy na thri operand, neu cyn dileu
  ailadroddus.
- `-v, --verbose` — adrodd am bob dileu fel `removed 'file'`.
- `--preserve-root` — gwrthod dileu `/` yn ailadroddus (y rhagosodiad).
- `--no-preserve-root` — caniatáu dileu `/`. Dim ond wedi'i sillafu'n
  llawn y'i derbynnir.
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `rm notes.txt` — dileu un ffeil.
- `rm -r Scratch` — dileu coeden `Scratch` a phopeth ynddi.
- `rm -I a b c d` — gofyn unwaith, yna dileu'r pedair ffeil ar `y`.

## EXIT STATUS

- `0` — llwyddodd pob dileu (nid yw cwestiwn a wrthodwyd na hepgoriad
  `-f` yn fethiannau).
- `1` — ni ellid dileu llwybr, neu fe fethodd anogwr neu allbwn;
  argraffir pob rheswm ar y gwall safonol. Mae llinell orchymyn na
  ddeallwyd hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `cp`
- `ls`
- `mv`
