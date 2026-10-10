## NAME

play — chwarae ffeiliau sain

## SYNOPSIS

`play [option...] file...`

## DESCRIPTION

Yn chwarae pob ffeil yn ei thro ar allbwn. Caiff pob ffeil ei datgodio mewn
proses ynysig heb unrhyw awdurdod o gwbl, felly ar y gwaethaf gall ffeil
elyniaethus ddim ond dod â'i datgodio ei hun i ben: caiff ei hepgor gyda'r
rheswm wedi'i nodi, a chwaraeir gweddill y rhestr. Darllenir ffeiliau AU, WAV a FLAC, FLAC yn frodorol neu mewn Ogg.

Mae ffeiliau olynol o'r un gyfradd, yr un fformat sampl a'r un drefn sianeli yn
chwarae'n ddi-fwlch, mewn un ffrwd. Mae ffeil o siâp arall yn aros i'r hyn sydd
yn y ciw gael ei chwarae, ac yna'n agor ei ffrwd ei hun.

Pan fo'r mewnbwn safonol yn derfynell, mae `play` yn tynnu rhyngwyneb sgrin
lawn: y ffeil sy'n chwarae, ei safle, y lefel a mesurydd ar gyfer pob sianel, a'r
rhestr. Nid yw'r chwarae'n dibynnu arno. O'i anfon i'r cefndir, mae `play` yn
dal ati i chwarae ac yn rhoi'r derfynell yn ôl; o'i ddwyn i'r blaendir, mae'n
tynnu ei hun eto. Heb y rhyngwyneb, mae'n adrodd ei gynnydd mewn un llinell ar
allbwn gwallau terfynell.

Mae'r rhyngwyneb yn derbyn y bysellau hyn: mae Bwlch neu `p` yn oedi ac yn
ailddechrau; mae'r saethau chwith a de yn neidio deg eiliad; mae `n` neu `>` yn
mynd i'r ffeil nesaf; mae `b` neu `<` yn mynd yn ôl i ddechrau'r ffeil hon, neu
i'r un flaenorol yn ystod ei thair eiliad gyntaf; mae `+`, `=` neu'r saeth i
fyny yn dri desibel yn uwch, a `-`, `_` neu'r saeth i lawr dri yn dawelach; mae
`q` neu Ctrl-C yn stopio; mae Ctrl-Z yn atal, gan oedi'r ffrwd yn gyntaf.

Gwanhad yw lefel ffrwd: ni ellir codi ffrwd heibio'r raddfa lawn, felly nid yw
`--gain` na'r bysellau lefel yn mynd yn uwch na 0 dB. I chwarae'n uwch, codwch
gyfaint yr allbwn.

Ysgrifennir amser fel `[[HH:]MM:]SS[.fraction]` — `90`, `1:30`, `1:02:03` neu
`12.5`.

Ar y wybodaeth safonol (fd 3), mae `play` yn ysgrifennu cofnod `schema` ar gyfer
pob ffeil a chwaraeir, cofnod `omission` ar gyfer pob ffeil a hepgorwyd neu a
dorrwyd yn fyr, a chofnod `summary` ar y diwedd.

## OPTIONS

- `-q, --quiet` — dim rhyngwyneb a dim llinell gynnydd.
- `-v, --verbose` — fformat a hyd pob ffeil ar yr allbwn gwallau.
- `--ui, --no-ui` — tynnu'r rhyngwyneb, neu byth; gwrthodir `--ui` heb
  derfynell.
- `-d, --device <sink>` — yr allbwn: `audio:sink/default`,
  `audio:sink/<id>` ar gyfer y cychwyn hwn, neu `audio:sink/<location>`
  lle bynnag y mae'r ddyfais, fel y mae `--list-devices` yn eu henwi.
- `-g, --gain <dB>` — lefel y ffrwd, 0 neu is, i'r ganfed ran.
- `-s, --start <time>` — dechrau pob ffeil ar yr amser hwn.
- `-t, --duration <time>` — chwarae cymaint â hyn o bob ffeil.
- `-l, --loop[=N]` — chwarae'r rhestr N gwaith i gyd, neu heb N am byth.
- `--list-devices` — enwi allbynnau'r sesiwn hon, pob un wrth ei
  ddynodwr a'i leoliad, a gadael.
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn.
- `--version` — dangos y fersiwn, a gadael.

## EXAMPLES

- `play song.wav` — chwarae un ffeil, gyda'r rhyngwyneb ar derfynell.
- `play -q intro.au song.wav &` — chwarae rhestr yn y cefndir.
- `play -s 1:30 -t 20 song.wav` — chwarae ugain eiliad o funud a hanner i mewn.
- `play -l3 -g -6 loop.wav` — chwarae ffeil dair gwaith, chwe desibel yn is.
- `play --list-devices` — gweld yr allbynnau.

## EXIT STATUS

- `0` — chwaraewyd pob ffeil, neu stopiwyd y chwarae heb hepgor yr un.
- `1` — ni ellid chwarae ffeil, neu ni allai'r chwarae fynd yn ei flaen.
- `2` — ni ddeallwyd y llinell orchymyn.

## ENVIRONMENT

- `TERM` — y derfynell y mae'r rhyngwyneb yn tynnu ar ei chyfer.
- `LANG` — hoff iaith y cymorth byr (tag BCP-47 megis `fr-FR`).

## SEE ALSO

- `man`
