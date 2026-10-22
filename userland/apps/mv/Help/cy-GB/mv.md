## NAME

mv — symud (ailenwi) ffeiliau a chyfeiriaduron

## SYNOPSIS

`mv [-finvT] [-t dir] [--] source... dest`

## DESCRIPTION

Mae'n symud pob operand ffynhonnell i gyrchfan. Gydag un ffynhonnell a
chyrchfan nad yw'n enwi cyfeiriadur, ailenwir y ffynhonnell i'r union
lwybr hwnnw. Pan fo'r gyrchfan yn enwi cyfeiriadur sy'n bodoli — a bob
amser pan fo mwy nag un ffynhonnell — symudir pob ffynhonnell *i mewn*
i'r cyfeiriadur hwnnw o dan ei henw sail ei hun.

Mae symudiad o fewn un gyfrol yn ailenwi atomig sy'n cadw hunaniaeth y
nod. Mae symudiad ar draws cyfrolau yn cadw'r un addewid: caiff y copi
ei adeiladu wrth ochr y gyrchfan, ei ysgrifennu i'r ddisg a'i ailenwi
i'w le mewn un cam, a dim ond wedyn y caiff y ffynhonnell ei dileu —
felly mae'r gyrchfan yn dal yr hyn a ddaliai nes bod y symudiad yn
gyfan, ac nid yw methiant na chwalfa yn colli dim. Yn y naill achos
neu'r llall, dim ond cyfeiriadur gwag y mae cyfeiriadur yn ei ddisodli,
nid oes dim arall yn disodli cyfeiriadur, ac nid oes cyfeiriadur yn
symud i'w is-goeden ei hun.

Trosysgrifir cyrchfan sy'n bodoli yn ragosodedig, fe'i hepgorir o dan
`-n`, a gofynnir amdani ar ffrwd y gwall safonol o dan `-i` (mae
cwestiwn a wrthodwyd yn hepgor y symudiad hwnnw heb wall; ni thrinnir
ateb annarllenadwy byth fel cydsyniad). Mae'r methiant cyntaf yn atal
y rhediad cyn unrhyw operand diweddarach. Mae `--` yn gorffen dosrannu
opsiynau: mae pob ymresymiad diweddarach yn llwybr.

Mae'r copi'n cario tyllau pob ffeil, ei phriodoleddau estynedig — math
ffeil RISC OS, math a chrëwr Mac — ei did caniatâd a'i pherchennog lle
bynnag y mae'r gyrchfan yn eu cadw, ac mae'n symud dolen symbolaidd fel
dolen sy'n storio'r un targed, byth yr hyn y mae'n ei enwi. Daw ffeil na
chaiff y symudwr ei rhoi'n ôl i'w pherchennog yn eiddo i'r symudwr, heb
ei didau set-ID. Ni chaiff stampiau amser eu cario: nid oes galwad ar y
system hon sy'n eu gosod. Adroddir y priodoleddau na all y gyrchfan eu
cadw mewn un rhybudd pan ddaw'r rhediad i ben, ac mewn cofnod `omission`
ar y ffrwd wybodaeth safonol, cyn i'r ffynonellau ddiflannu am byth.

## OPTIONS

- `-f, --force` — peidio byth â gofyn cyn trosysgrifo. Mae'r olaf o
  `-f`, `-i` a `-n` yn ennill.
- `-i, --interactive` — gofyn cyn trosysgrifo cyrchfan sy'n bodoli;
  dim ond ateb yn dechrau ag `y`/`Y` sy'n cydsynio.
- `-n, --no-clobber` — peidio byth â throsysgrifo cyrchfan sy'n
  bodoli.
- `-v, --verbose` — adrodd pob symudiad fel
  `renamed 'source' -> 'dest'`, a symudiad ar draws cyfrolau fel
  `copied 'source' -> 'dest'` ac yna `removed 'source'`.
- `-t dir, --target-directory=dir` — symud pob ffynhonnell i `dir`, y
  mae'n rhaid iddo fod yn gyfeiriadur sy'n bodoli. Daw'r gwerth
  ynghlwm (`-tdir`, `--target-directory=dir`) neu fel yr ymresymiad
  nesaf.
- `-T, --no-target-directory` — trin y gyrchfan fel ffeil gyffredin;
  caniateir un ffynhonnell yn union. Ni ellir ei gyfuno ag `-t`.
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `mv draft.txt final.txt` — ailenwi un ffeil.
- `mv -v a.txt b.txt Archive` — symud y ddwy ffeil i `Archive`, gan
  adrodd am bob symudiad.
- `mv -n new.cfg current.cfg` — gosod ffeil dim ond os nad yw'r
  gyrchfan yn bodoli eisoes.

## EXIT STATUS

- `0` — llwyddodd pob symudiad (nid yw hepgoriad `-n` na chwestiwn
  `-i` a wrthodwyd yn fethiannau).
- `1` — methiant system ffeiliau, anogwr neu allbwn; argraffir y
  rheswm ar y gwall safonol.
- `2` — ni ddeallwyd y llinell orchymyn.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
