## NAME

gpucheck — gwirio pob GPU a weinyddir yn erbyn y ddyfais feddalwedd

## SYNOPSIS

`gpucheck`

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
ateb drwy roi'r gorau i'r lluniadu. Yn olaf, mae'n gofyn i'r GPU redeg
rhaglen dros fwy o grwpiau o waith nag y mae'n eu caniatáu, y mae'n
rhaid i bob dyfais ei ateb drwy roi'r gorau i'r gwaith hwnnw. Mae GPU yn
cytuno pan fydd yn gadael yr un beitiau â'r ddyfais feddalwedd, yn
gwrthod y llenwad hwnnw am yr un rheswm, ac yn rhoi'r gorau i'r un
gwaith.

Mae'r gorchymyn yn argraffu un llinell ar gyfer pob GPU, gan enwi ei
nod caledwedd: naill ai bod y GPU yn cytuno, neu'r gwahaniaeth cyntaf a
ganfuwyd — y gwaith wedi'i wrthod, ac ar ba gam; beit sy'n darllen yn
wahanol; ateb gwahanol i'r llenwad; gwerth a adawodd rhaglen graffeg yn
wahanol; beit o lun sy'n darllen yn wahanol; beit o luniad sy'n darllen
yn wahanol; ateb gwahanol i'r lluniadu o restr; neu ateb gwahanol i'r
gwaith heibio i derfynau'r GPU.

Mae `sysinfo gpu` yn rhestru'r GPUau y mae'r gorchymyn yn eu gwirio.

## OPTIONS

- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `gpucheck` — gwirio pob GPU a weinyddir.

## EXIT STATUS

- `0` — cytunodd pob GPU a weinyddir â'r ddyfais feddalwedd.
- `1` — roedd GPU yn wahanol neu ni ellid ei gyrraedd, ni weinyddir
  unrhyw GPU, ni ellid rhestru'r GPUau a weinyddir, neu ni allai'r
  ddyfais feddalwedd redeg y gwaith. Mae llinell orchymyn na ddeallwyd hefyd yn gorffen
  â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
