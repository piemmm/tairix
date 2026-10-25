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
ei adeiladu wrth ochr y gyrchfan, yn ddarllenadwy i'w berchennog yn unig
nes ei fod yn gyfan, ei ysgrifennu i'r ddisg a'i ailenwi i'w le mewn un
cam, a dim ond wedyn y caiff yr hyn a gopïwyd ei ddileu — felly mae'r
gyrchfan yn dal yr hyn a ddaliai nes bod y symudiad yn gyfan, ac nid yw
methiant na chwalfa yn colli dim. Caiff ffeil a ychwanegwyd at y
ffynhonnell, neu un a newidiwyd, tra oedd yn cael ei chopïo ei chadw
lle'r oedd a'i hadrodd, ynghyd â'r cyfeiriadur sy'n ei dal; ar gyfrol
nad yw'n cofnodi cyfrif newidiadau, neu sy'n cadw amserau bras (dwy
eiliad yn achos FAT), gall ailysgrifennu o'r un hyd o fewn y cydraniad
hwnnw fynd heb ei weld. Yn y naill achos neu'r llall, dim ond
cyfeiriadur gwag y mae cyfeiriadur yn ei ddisodli, nid oes dim arall yn
disodli cyfeiriadur, ac nid oes cyfeiriadur yn symud i'w is-goeden ei
hun.

Trosysgrifir cyrchfan sy'n bodoli yn ragosodedig, fe'i cedwir o dan `-n`
(sy'n dweud `not replacing` ac yn peri i'r rhediad fethu, fel un GNU), a
gofynnir amdani ar ffrwd y gwall safonol o dan `-i` (mae cwestiwn a
wrthodwyd yn hepgor y symudiad hwnnw ac yn peri i'r rhediad fethu; ni
thrinnir ateb annarllenadwy byth fel cydsyniad). Adroddir am operand na
ellir ei symud, a symudir y nesaf serch hynny. Mae `--` yn gorffen
dosrannu opsiynau: mae pob ymresymiad diweddarach yn llwybr.

Mae'r copi'n cario tyllau pob ffeil, ei phriodoleddau estynedig — math
ffeil RISC OS, math a chrëwr Mac — ei did caniatâd a'i pherchennog lle
bynnag y mae'r gyrchfan yn eu cadw, ac mae'n symud dolen symbolaidd fel
dolen sy'n storio'r un targed, byth yr hyn y mae'n ei enwi. Mae ffeil a
roddir yn ôl i berchennog arall yn cyrraedd heb ei didau set-ID, y mae'r
system yn eu clirio wrth i berchennog newid; daw un na chaiff y symudwr
ei rhoi'n ôl yn eiddo i'r symudwr, yn ei grŵp ei hun lle caiff y symudwr
ei gadw, a heb ddidau set-ID perchennog neu grŵp na chadwodd. Ni chaiff
stampiau amser eu cario: nid oes galwad ar y system hon sy'n eu gosod.
Adroddir y priodoleddau na all y gyrchfan eu cadw mewn un rhybudd pan
ddaw'r rhediad i ben, ac mewn cofnod `omission` ar y ffrwd wybodaeth
safonol, cyn i'r ffynonellau ddiflannu am byth.

## OPTIONS

- `-f, --force` — peidio byth â gofyn cyn trosysgrifo. Mae'r olaf o
  `-f`, `-i` a `-n` yn ennill.
- `-i, --interactive` — gofyn cyn trosysgrifo cyrchfan sy'n bodoli;
  dim ond ateb yn dechrau ag `y`/`Y` sy'n cydsynio.
- `-n, --no-clobber` — peidio byth â throsysgrifo cyrchfan sy'n bodoli;
  adroddir pob un a gedwir fel `not replacing 'dest'`.
- `-v, --verbose` — adrodd pob symudiad fel
  `renamed 'source' -> 'dest'`, a symudiad ar draws cyfrolau gofnod wrth
  gofnod, fel y gwna GNU: `created directory 'dest'` ar gyfer pob
  cyfeiriadur a `copied 'source' -> 'dest'` ar gyfer popeth arall wrth
  ei gopïo, ac yna `removed 'source'` neu `removed directory 'source'`
  ar gyfer pob un wrth i'r ffynhonnell fynd.
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
- `mv -n new.cfg current.cfg` — gosod ffeil dim ond os nad yw'r gyrchfan
  yn bodoli eisoes (gan ddweud `not replacing` os ydyw).

## EXIT STATUS

- `0` — llwyddodd pob symudiad.
- `1` — ni ellid symud operand, cadwodd `-n` gyrchfan, gwrthodwyd
  cwestiwn `-i`, newidiodd rhywbeth mewn ffynhonnell tra oedd yn cael ei
  symud a chafodd ei gadw, neu fe fethodd anogwr neu allbwn; argraffir
  rheswm pob methiant ar y gwall safonol. Mae llinell orchymyn na
  ddeallwyd hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
