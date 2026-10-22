## NAME

cp — copïo ffeiliau a chyfeiriaduron

## SYNOPSIS

`cp [-dfilnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

## DESCRIPTION

Mae'n copïo pob operand ffynhonnell i gyrchfan. Gydag un ffynhonnell a
chyrchfan nad yw'n enwi cyfeiriadur, copïir y ffynhonnell i'r union
lwybr hwnnw. Pan fo'r gyrchfan yn enwi cyfeiriadur sy'n bodoli — a bob
amser pan fo mwy nag un ffynhonnell — copïir pob ffynhonnell *i mewn*
i'r cyfeiriadur hwnnw o dan ei henw sail ei hun.

Dim ond gydag `-r` y copïir ffynhonnell sy'n gyfeiriadur, sy'n
atgynhyrchu'r is-goeden gyfan; heb `-r` gwrthodir operand cyfeiriadur.
Trosysgrifir ffeil gyrchfan sy'n bodoli yn ragosodedig, fe'i hepgorir
o dan `-n`, a gofynnir amdani ar ffrwd y gwall safonol o dan `-i`
(mae cwestiwn a wrthodwyd yn hepgor y copi hwnnw heb wall; ni thrinnir
ateb annarllenadwy byth fel cydsyniad).
Gwrthodir cyrchfan sy'n ffeil y ffynhonnell ei hun — yr un enw, enw
arall arni, neu ddolen iddi — fel yr un ffeil, a'i gadael fel yr oedd.

Mae'r methiant cyntaf yn atal y rhediad cyn unrhyw operand
diweddarach. Mae `--` yn gorffen dosrannu opsiynau: mae pob ymresymiad
diweddarach yn llwybr.

Mae pob copi'n cario priodoleddau estynedig y ffynhonnell — math ffeil
RISC OS, math a chrëwr Mac — lle bynnag y gall y cyrchfan eu cadw, ac mae'n
cadw tyllau ffeil denau yn dyllau. Adroddir y priodoleddau na all cyrchfan
eu cadw mewn un rhybudd pan ddaw'r rhediad i ben, ac mewn cofnod `omission`
ar y ffrwd wybodaeth safonol; gyda `--preserve=xattr` mae copi o'r fath yn
methu yn lle hynny.

## OPTIONS

- `-r, -R, --recursive` — copïo cyfeiriaduron a'u cynnwys.
- `-f, --force` — pan na ellir creu ffeil gyrchfan, ei dileu a
  cheisio'r copi unwaith eto.
- `-i, --interactive` — gofyn cyn trosysgrifo ffeil sy'n bodoli; dim
  ond ateb yn dechrau ag `y`/`Y` sy'n cydsynio.
- `-n, --no-clobber` — peidio byth â throsysgrifo ffeil sy'n bodoli.
  Y diweddaraf o `-i` ac `-n` sy'n ennill.
- `-l, --link` — rhoi i'r cyrchfan ail enw i nod y ffynhonnell yn lle
  copïo ei beitiau, fel na all y ddau enw wahanu wrth ysgrifennu'n
  ddiweddarach. Mae angen `-r` ar ffynhonnell sy'n gyfeiriadur o hyd.
- `-s, --symbolic-link` — creu cyswllt symbolaidd sy'n enwi'r ffynhonnell
  yn lle ei chopïo.
- `-P, --no-dereference` — atgynhyrchu ffynhonnell sy'n gyswllt
  symbolaidd fel cyswllt sy'n storio'r un targed, air am air, yn lle
  copïo'r hyn y mae'n ei enwi (felly mae cyswllt cymharol neu grog yn
  goroesi'r copi). Hebddi, dilynir cyswllt ffynhonnell.
- `--preserve=links` — mae dwy ffynhonnell sy'n enwi un nod yn cael dau
  enw yn y cyrchfan yn lle dau gopi, fel nad yw'r copi'n dyblu'r storfa'n
  ddistaw.
- `--preserve=xattr` — rhaid cario pob priodoledd estynedig: mae copi na all
  ei gyrchfan gadw un yn methu, gan ei enwi. Mae `--preserve=links,xattr` yn
  gofyn am y ddau.
- `--no-preserve=xattr` — peidio â chopïo unrhyw briodoledd estynedig.
  Derbynnir `mode`, `ownership` a `timestamps` hefyd, ac nid ydynt yn cael eu
  cadw beth bynnag.
- `--sparse=WHEN` — sut y copïir ffeil denau: mae `auto`, y rhagosodiad, yn
  cadw ei thyllau; mae `always` hefyd yn gadael heb ei ysgrifennu bob bloc o
  sero y mae'n ei storio; mae `never` yn ysgrifennu pob beit.
- `-d` — `-P` a `--preserve=links` gyda'i gilydd, fel yn yr offeryn GNU.
- `-v, --verbose` — adrodd am bob copi fel `'source' -> 'dest'`.
- `-t dir, --target-directory=dir` — copïo pob ffynhonnell i `dir`,
  y mae'n rhaid iddo fod yn gyfeiriadur sy'n bodoli. Daw'r gwerth
  ynghlwm (`-tdir`, `--target-directory=dir`) neu fel yr ymresymiad
  nesaf.
- `-T, --no-target-directory` — trin y gyrchfan fel ffeil gyffredin;
  caniateir un ffynhonnell yn union. Ni ellir ei gyfuno ag `-t`.
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `cp notes.txt backup.txt` — copïo un ffeil i enw newydd.
- `cp -r Projects Archive` — atgynhyrchu coeden `Projects` y tu mewn i
  `Archive` (neu fel `Archive` os nad yw'n bodoli).
- `cp -v -t Backup a.txt b.txt` — copïo'r ddwy ffeil i `Backup`, gan
  adrodd am bob copi.

## EXIT STATUS

- `0` — llwyddodd pob copi (nid yw hepgoriad `-n` na chwestiwn `-i` a
  wrthodwyd yn fethiannau).
- `1` — methiant system ffeiliau, anogwr neu allbwn; argraffir y
  rheswm ar y gwall safonol.
- `2` — ni ddeallwyd y llinell orchymyn.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
