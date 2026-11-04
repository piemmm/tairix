## NAME

unmount — datgysylltu cyfrol wedi'i gosod

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Yn tynnu'r gyfrol sydd wedi'i gosod dan `name` o wasanaeth: caiff y
system ffeiliau a'r ddyfais eu gwagio, tynnir y gosodiad dan
`/Storage`, a dirymir gwreiddyn parhaol `id::` y gyfrol. `name` yw
enw catalog y gyfrol (`usb1`), cyfeiriadur y mae wedi'i gosod arno
(`/Storage/usb1`, neu'r un y rhwymodd `mount delwedd cyfeiriadur` hi
iddo), neu enw ffeil y ddelwedd ddisg y'i gosodwyd ohoni
(`Workbench.adf`), wedi'i gymharu â rhestrau gosodiadau a golygon API
gwybodaeth y system; mae enw ffeil a rennir gan ddelwedd defnyddiwr
arall yn enwi'ch un chi. Nid yw enw ffeil a allai olygu mwy nag un
gyfrol, nac un nad yw'r rhestr yn cadw ond ei ddechrau, yn enwi'r un:
enwch y gyfrol wrth y man lle mae wedi'i gosod.
Gall llwybr fod yn gymharol neu orffen â slaes: caiff ei ddatrys fel y
mae `mount` yn datrys un. Nid yw cyfrol breifat defnyddiwr arall yn
dangos enw ei ffeil na'r cyfeiriaduron y'i rhwymwyd iddynt, felly fe'i
henwir wrth y man lle mae wedi'i gosod yn `/Storage`.

Mae cyfrol y tynnwyd ei dyfais tra oedd ysgrifeniadau heb eu cadarnhau
yn aros yn weladwy fel `unavailable-dirty` yn y rhestr osodiadau, ac
mae `unmount` plaen yn gwrthod: cedwir ei data wedi'i gadw ar gyfer
ailfewnosod wedi'i wirio. Mae'r un peth yn wir am gyfrol a roddwyd yn
ôl na ellid dangos ei bod heb ei newid (`recovery-conflict`), sy'n dal
i gadw'r data hwnnw, ac am un na ellid cadw ei hysgrifeniadau heb eu
cadarnhau (`unavailable-lost`), fel bod eu colled yn cael ei chydnabod
yn hytrach na'i hanwybyddu. Mae cyfrol sydd â
ffeiliau'n dal ar agor arni, neu y mae rhaglen yn ei defnyddio fel ei
chyfeiriadur gwaith, yn gwrthod `unmount` plaen hefyd. `--force` yw'r
allanfa fwriadol — caiff ffeiliau sy'n dal ar agor ar y gyfrol eu
torri i ffwrdd, caiff rhaglen sy'n gweithio ynddi ei gadael mewn
cyfeiriadur nad yw'n bodoli mwyach, caiff y data a gadwyd ei daflu,
tynnir y gyfrol, a chofnodir yr hyn a ildiwyd yn y log archwilio. Ar
gyfrol iach mae `--force` yn dal i wagio a datgysylltu'n lân; ni
thaflir dim pan fo cadarnhad glân yn bosibl.

Cewch ddatgysylltu delwedd ddisg neu gyfran rwydwaith a osodoch chi
eich hun, a chewch ddatgysylltu disg symudadwy — cof bach USB, cerdyn
cof — tra byddwch chi'n defnyddio'r cyfrifiadur ei hun, ond dim ond
yn blaen, byth â `--force`; mae unrhyw gyfrol arall yn gofyn am yr
awdurdod gosod (`CAP_FS_MOUNT`). Mae'r cnewyllyn yn ei wirio ac yn
archwilio pob
penderfyniad. Nid yw'r cyfrolau cychwyn parhaol na rhwymiadau golwg y
system yn ddatgysylltadwy.

## OPTIONS

- `-f, --force` — dad-osod gorfodol: tynnu'r gyfrol hyd yn oed tra bo
  ffeiliau ar agor arni, tra bo rhaglen yn gweithio ynddi, neu pan na
  ellir cadarnhau ei data sydd heb ei gadarnhau, gan dorri'r ffeiliau
  agored i ffwrdd a thaflu'r data a gadwyd.
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn.

## EXAMPLES

- `unmount usb1` — datgysylltu'n lân y gyfrol a osodwyd fel `usb1`.
- `unmount /Storage/usb1` — yr un peth, wedi'i enwi wrth ei lwybr
  gosod.
- `unmount Workbench.adf` — datgysylltu'r ddelwedd ddisg a osodwyd o
  `Workbench.adf`.
- `unmount --force usb1` — tynnu cyfrol nad yw ar gael gan daflu ei
  data a gadwyd heb ei gadarnhau.

## EXIT STATUS

- `0` — datgysylltwyd y gyfrol (neu ysgrifennwyd y cymorth byr).
- `1` — ni ddeallwyd y llinell orchymyn, neu ni chaiff y galwr
  ddatgysylltu'r gyfrol.
- `2` — methodd y gwasanaeth gwybodaeth system, neu ysgrifennu i'r
  derfynell.
- `32` — nid oedd y gyfrol wedi'i mowntio neu nid yw'n ddatgysylltadwy,
  ni ellid datrys y llwybr sy'n ei henwi, neu methodd ei datgysylltu.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `mount`
- `df`
- `man`
