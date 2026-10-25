## NAME

unmount — datgysylltu cyfrol wedi'i gosod

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Yn tynnu'r gyfrol sydd wedi'i gosod dan `name` o wasanaeth: caiff y
system ffeiliau a'r ddyfais eu gwagio, tynnir y gosodiad dan
`/Storage`, a dirymir gwreiddyn parhaol `id::` y gyfrol. `name` yw
enw catalog y gyfrol (`usb1`) neu ei llwybr gosod (`/Storage/usb1`),
wedi'i gymharu â rhestr osodiadau API gwybodaeth y system.

Mae cyfrol y tynnwyd ei dyfais tra oedd ysgrifeniadau heb eu cadarnhau
yn aros yn weladwy fel `unavailable-dirty` (neu `unavailable-lost`) yn
y rhestr osodiadau, ac mae `unmount` plaen yn gwrthod: cedwir ei data
wedi'i gadw ar gyfer ailfewnosod wedi'i wirio. Mae cyfrol sydd â
ffeiliau'n dal ar agor arni, neu y mae rhaglen yn ei defnyddio fel ei
chyfeiriadur gwaith, yn gwrthod `unmount` plaen hefyd. `--force` yw'r
allanfa fwriadol — caiff ffeiliau sy'n dal ar agor ar y gyfrol eu
torri i ffwrdd, caiff rhaglen sy'n gweithio ynddi ei gadael mewn
cyfeiriadur nad yw'n bodoli mwyach, caiff y data a gadwyd ei daflu,
tynnir y gyfrol, a chofnodir yr hyn a ildiwyd yn y log archwilio. Ar
gyfrol iach mae `--force` yn dal i wagio a datgysylltu'n lân; ni
thaflir dim pan fo cadarnhad glân yn bosibl.

Mae datgysylltu yn gofyn am yr awdurdod gosod (`CAP_FS_MOUNT`); mae'r
cnewyllyn yn ei wirio ac yn archwilio pob penderfyniad. Nid yw'r
cyfrolau cychwyn parhaol na rhwymiadau golwg y system yn
ddatgysylltadwy.

## OPTIONS

- `-f, --force` — dad-osod gorfodol: tynnu'r gyfrol hyd yn oed tra bo
  ffeiliau ar agor arni, tra bo rhaglen yn gweithio ynddi, neu pan na
  ellir cadarnhau ei data sydd heb ei gadarnhau, gan dorri'r ffeiliau
  agored i ffwrdd a thaflu'r data a gadwyd.
- `-?, --help` — dangos cymorth byr y gorchymyn hwn.

## EXAMPLES

- `unmount usb1` — datgysylltu'n lân y gyfrol a osodwyd fel `usb1`.
- `unmount /Storage/usb1` — yr un peth, wedi'i enwi wrth ei lwybr
  gosod.
- `unmount --force usb1` — tynnu cyfrol nad yw ar gael gan daflu ei
  data a gadwyd heb ei gadarnhau.

## EXIT STATUS

- `0` — datgysylltwyd y gyfrol (neu ysgrifennwyd y cymorth byr).
- `1` — ni chafwyd hyd i'r gyfrol, nid yw'n ddatgysylltadwy, neu
  gwrthododd y cnewyllyn y datgysylltiad. Mae llinell orchymyn na
  ddeallwyd hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `mount`
- `df`
- `man`
