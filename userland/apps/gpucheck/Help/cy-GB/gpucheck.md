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
ddiwedd byffer, y mae'n rhaid i bob dyfais ei wrthod. Mae GPU yn cytuno
pan fydd yn gadael yr un beitiau â'r ddyfais feddalwedd ac yn gwrthod y
llenwad hwnnw am yr un rheswm.

Mae'r gorchymyn yn argraffu un llinell ar gyfer pob GPU, gan enwi ei
nod caledwedd: naill ai bod y GPU yn cytuno, neu'r gwahaniaeth cyntaf a
ganfuwyd — beit sy'n darllen yn wahanol, y gwaith wedi'i wrthod, neu
ateb gwahanol i'r llenwad.

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
