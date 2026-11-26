## NAME

gpucheck — gwirio pob GPU a weinyddir, y sgrin sy'n dangos yr hyn y mae
un yn ei luniadu, a chnewyllyn cyfrifo o'ch eiddo eich hun

## SYNOPSIS

`gpucheck`

`gpucheck --present`

`gpucheck --shader FILE [--words N]`

## DESCRIPTION

Mae'n rhedeg yr un darn byr o waith ar bob GPU y mae gyrrwr yn ei
weinyddu ac ar ddyfais feddalwedd y system, ac yn dweud ar gyfer pob GPU
a yw'r ddau'n cytuno.

Mae'r gwaith yn ysgrifennu patrwm hysbys i gof y gall y gorchymyn ei
ddarllen, yn llenwi'r cof wrth ei ochr, yn copïo'r ddau drwy gof y mae'r
GPU yn ei gadw iddo'i hun ac yn ôl, ac yna'n gofyn am lenwad heibio i
ddiwedd byffer, y mae'n rhaid i bob dyfais ei wrthod. Yna mae'n rhedeg
pob un o raglenni graffeg y system ei hun ar lun hysbys. Os gall y GPU
ddal pob math o lun y mae'r cam nesaf yn ei ddefnyddio, mae'r gwaith yn
copïo lluniau i mewn i'r cof ac allan ohono ac o un llun i'r llall, yn
eu clirio, ac yn rhedeg rhaglen sy'n darllen, yn ysgrifennu ac yn samplu
lluniau. Os gall y GPU luniadu, mae'r gwaith yn lluniadu siapiau,
pwyntiau a llinell o liwiau hysbys i mewn i luniau, gan guddio'r hyn
sydd y tu ôl, gan hepgor siapiau sy'n wynebu i ffwrdd, gan gymysgu
lliwiau a chadw rhai heb eu cyffwrdd, ac yn casglu pedwar sampl o bob
picsel yn un. Mae hefyd yn lluniadu o restr yn y cof sy'n enwi'r copi
cyntaf o siâp i'w luniadu, y mae'n rhaid i GPU na all wneud hynny ei
ateb drwy roi'r gorau i'r lluniadu. Wedyn, mae'n gofyn i'r GPU redeg
rhaglen dros fwy o grwpiau o waith nag y mae'n eu caniatáu, y mae'n
rhaid i bob dyfais ei ateb drwy roi'r gorau i'r gwaith hwnnw. Yn olaf,
mae'n rhedeg rhaglenni bach sydd rhyngddynt yn defnyddio pob cyfarwyddyd
y caiff rhaglen GPU ei ddefnyddio, a rhaid i bob ateb gyfateb i ateb y
ddyfais feddalwedd mor fanwl ag y mae diffiniad ei gyfarwyddyd yn ei
warantu; ni chaiff GPU na all redeg un ohonynt ei farnu arni. Mae GPU yn
cytuno pan fydd yn gadael yr un beitiau â'r ddyfais feddalwedd, yn
gwrthod y llenwad hwnnw am yr un rheswm, yn rhoi'r gorau i'r un gwaith,
ac yn ateb pob un o'r rhaglenni hynny yn yr un modd.

Mae'r gorchymyn yn argraffu un llinell ar gyfer pob GPU, gan enwi ei
nod caledwedd: naill ai bod y GPU yn cytuno, neu'r gwahaniaeth cyntaf a
ganfuwyd — y gwaith wedi'i wrthod, ac ar ba gam; beit sy'n darllen yn
wahanol; ateb gwahanol i'r llenwad; gwerth a adawodd rhaglen graffeg yn
wahanol; beit o lun sy'n darllen yn wahanol; beit o luniad sy'n darllen
yn wahanol; ateb gwahanol i'r lluniadu o restr; ateb gwahanol i'r gwaith
heibio i derfynau'r GPU; neu gyfarwyddyd a roddodd, yn un o'r rhaglenni
bach, ateb ymhellach oddi wrth ateb y ddyfais feddalwedd nag y mae ei
ddiffiniad yn ei ganiatáu.

Mae `sysinfo gpu` yn rhestru'r GPUau y mae'r gorchymyn yn eu gwirio.

Gyda `--present` mae'r gorchymyn yn dangos llun hysbys yn lle hynny:
cefndir glas, stribed o liwiau, rhes o deils sgwarog ac un deilsen
wedi'i chopïo i'r gornel bellaf, mewn ffenestr sy'n gorchuddio'r sgrin
gyfan. Y GPU cyntaf y mae gyrrwr yn ei weinyddu sy'n ei luniadu, neu'r
ddyfais feddalwedd pan na weinyddir yr un. Mae'r gorchymyn yn cyflwyno'r
llun unwaith bob ffrâm nes pwyso Escape neu gau'r ffenestr, ac yna'n
argraffu sawl ffrâm a gyflwynodd, beth a'u lluniadodd, ac a ddangosodd y
sgrin bob un.

Gyda `--shader` mae'r gorchymyn yn rhedeg rhaglen o'ch eiddo eich hun yn
lle hynny: y cnewyllyn cyfrifo yn FILE, wedi'i ysgrifennu yn WGSL neu
wedi'i grynhoi i SPIR-V, a dechrau'r ffeil sy'n dangos pa un ydyw. Caiff
WGSL ei grynhoi mewn proses ar wahân na all gyrraedd dim byd arall, a
chaiff y naill fath neu'r llall ei wirio cyn ei roi i unrhyw GPU. Ni
chaiff cnewyllyn a wrthodir ei redeg: caiff y gwrthodiad ei argraffu,
gyda'r llinell a'r golofn lle mae'n gorwedd yn achos WGSL. Rhaid i'r
cnewyllyn fod ag un pwynt mynediad cyfrifo, sy'n defnyddio un byffer
storio yng ngrŵp 0, rhwymiad 0. Mae'r byffer yn dal N gair, 256 oni bai
bod `--words` yn dweud fel arall, gyda gair *i* yn dechrau â'r gwerth
*i*, ac mae'r cnewyllyn yn rhedeg unwaith ar gyfer pob gair. Mae'n
rhedeg ar bob GPU y mae gyrrwr yn ei weinyddu ac ar y ddyfais
feddalwedd; wrth ei ochr, mewn cof sy'n perthyn i raglen ar wahân, mae'r
GPU yn cadw marciwr na ddylai byffer y cnewyllyn byth ddod i'w ddal. Ar
gyfer pob GPU mae'r gorchymyn yn argraffu naill ai bod y ddau'n cytuno —
mae'r byffer yn darllen yr un fath, neu fe roddodd y ddau'r gorau i
gnewyllyn a redodd heibio i'w derfyn amser, gan barhau i weinyddu wedyn
— neu'r gwahaniaeth cyntaf: gair sy'n darllen yn wahanol, y marciwr
wedi'i ganfod yn y byffer, y GPU yn gwrthod y cnewyllyn, yn rhoi'r gorau
iddo neu'n ei adael heb ei ateb lle y rhedodd y ddyfais feddalwedd ef,
neu GPU na weinyddodd ddim byd ar ôl rhoi'r gorau iddo.

## OPTIONS

- `--present` — dangos y llun ar sgrin lawn, a gwirio bod y sgrin wedi
  dangos pob ffrâm a gyflwynwyd.
- `--shader FILE` — rhedeg y cnewyllyn cyfrifo yn FILE, cod ffynhonnell
  WGSL neu fodiwl SPIR-V, ar bob GPU a weinyddir ac ar y ddyfais
  feddalwedd.
- `--words N` — gyda `--shader`, sawl gair y mae byffer y cnewyllyn yn
  eu dal: o 1 i 65536, a 256 pan na roddir gwerth.
- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `gpucheck` — gwirio pob GPU a weinyddir.
- `gpucheck --present` — dangos y llun ar sgrin lawn; pwyso Escape i
  stopio.
- `gpucheck --shader double.wgsl` — rhedeg cnewyllyn WGSL dros 256 gair
  ar bob GPU a weinyddir.
- `gpucheck --shader kernel.spv --words 4096` — rhedeg cnewyllyn wedi'i
  grynhoi dros 4096 gair.

## EXIT STATUS

- `0` — cytunodd pob GPU a weinyddir â'r ddyfais feddalwedd; gyda
  `--present`, dangosodd y sgrin bob ffrâm a gyflwynwyd; gyda
  `--shader`, cytunodd pob GPU a weinyddir â'r ddyfais feddalwedd ar y
  cnewyllyn.
- `1` — roedd GPU yn wahanol neu ni ellid ei gyrraedd, ni weinyddir
  unrhyw GPU, ni ellid rhestru'r GPUau a weinyddir, neu ni allai'r
  ddyfais feddalwedd redeg y gwaith; gyda `--present`, roedd ffrâm na
  ddangoswyd, ni chyflwynwyd yr un, neu fe wrthododd y ffenestr neu'r
  ddyfais gam; gyda `--shader`, ni ellid darllen y ffeil, gwrthodwyd y
  cnewyllyn, neu roedd GPU yn wahanol. Mae llinell orchymyn na ddeallwyd
  hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
