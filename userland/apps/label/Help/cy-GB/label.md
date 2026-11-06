## NAME

label — dangos neu newid teitl cyfrol

## SYNOPSIS

`label [option...] name [title]`

## DESCRIPTION

Yn dangos teitl y gyfrol y mae `name` yn ei henwi, neu, o roi `title`,
yn ei osod. Teitl cyfrol yw'r label y mae ei system ffeiliau yn ei
chofnodi: yr enw y'i dangosir wrtho, a'r enw a gaiff yn `/Storage` pan
gaiff ei mowntio nesaf. `name` yw enw catalog y gyfrol (`usb1`),
cyfeiriadur y mae wedi'i mowntio arno (`/Storage/usb1`), enw ffeil y
ddelwedd ddisg y'i mowntiwyd ohoni (`Workbench.adf`), neu ei gwraidd
parhaol (`id::…`), wedi'u henwi fel ar gyfer `unmount`. Mae `title`
gwag yn clirio'r teitl lle mae fformat y gyfrol yn caniatáu hynny.

Tra bo'r gyfrol yn aros wedi'i mowntio, nid yw ei henw yn `/Storage` yn
newid, felly nid oes dim sy'n ei henwi yno yn peidio â gweithio; mae'r
teitl newydd yn ei henwi o'i mowntio nesaf ymlaen. Mae ei hunaniaeth,
a hunaniaeth ei ffeiliau, yn aros fel yr oeddent.

Mae pob fformat yn sillafu teitl yn ei ffordd ei hun, a gwrthodir teitl
na all ei storio yn hytrach na'i fyrhau. Mae cyfrol FAT yn derbyn hyd
at un ar ddeg o briflythrennau, digidau, bylchau a'r atalnodi y mae
enw ffeil byr yn ei ganiatáu, heb ddechrau na gorffen â bwlch; cyfrol
ext2, ext3 neu ext4 hyd at un ar bymtheg beit; cyfrol ADFS hyd at ddeg
nod; cyfrol AmigaDOS o un i ddeg ar hugain; cyfrol UDF hyd at 126; a
chyfrol ARXFS hyd at 252 beit, oni bai ei bod wedi'i hamgryptio. Nid
yw disg a feistrolwyd â theitl, archif na chyfran rhwydwaith yn derbyn
un newydd.

Mae delwedd ddisg a fowntiwyd gennych chi eich hun yn eiddo i chi i'w
hailenwi. Mae disg symudadwy — cof bach USB, cerdyn — yn eiddo i chi
i'w ailenwi tra byddwch yn defnyddio'r cyfrifiadur ei hun, os cewch
newid yr hyn sydd yn ei ffolder uchaf; mae angen yr awdurdod mowntio
(`CAP_FS_MOUNT`), sydd gan weinyddwr, ar unrhyw gyfrol arall. Mae'r
cnewyllyn yn gwirio hyn ac yn cofnodi pob penderfyniad yn y log
archwilio.

## OPTIONS

- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.
- `--version` — dangos enw a fersiwn y gorchymyn hwn.

## EXAMPLES

- `label usb1` — dangos teitl y gyfrol a fowntiwyd fel `usb1`.
- `label usb1 HOLIDAY` — rhoi'r teitl `HOLIDAY` iddi.
- `label /Storage/backup 'Backup 2026'` — rhoi teitl i'r gyfrol a
  fowntiwyd ar `/Storage/backup`.
- `label usb1 ''` — clirio ei theitl.

## EXIT STATUS

- `0` — dangoswyd neu gosodwyd y teitl (neu ysgrifennwyd y cymorth neu'r
  fersiwn).
- `1` — ni ddeallwyd y llinell orchymyn, nid oedd yr enw yn enwi unrhyw
  gyfrol, nid oedd y teitl yn deitl, neu gwrthododd y gyfrol ef.
- `2` — methodd y gwasanaeth gwybodaeth system, neu ysgrifennu i'r
  derfynell.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 megis
  `fr-FR`).

## SEE ALSO

- `mount`
- `unmount`
- `df`
